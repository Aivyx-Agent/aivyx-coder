# BUSL-1.1 Relicense — Part 1: `aivyx-coder` Design

## Context

`aivyx-pa` already relicensed its entire public workspace from MIT to the
Business Source License 1.1 (BUSL-1.1) in its own Chapter Charter (Amendment
A14, 2026-06-19) — free for personal/individual/non-commercial/educational/
research use, a paid commercial license for any for-profit/production/
revenue use, auto-reverting to MIT four years after each release (per-release
clock). `aivyx-ecosystem` then built the shared legal groundwork ("Part 0",
2026-09-23/24, merged) so the rest of the ecosystem could adopt the same
posture without re-drafting legal text per repo: canonical templates now live
at `aivyx-ecosystem/docs/legal/` (`busl-license-template.md`, `CLA.md`,
`CONTRIBUTING-cla-section.md`, `COMMERCIAL.md`, `TRADEMARK.md`), plus a
root-level `LICENSING.md` documenting the ecosystem-wide policy.

This is Part 1 of that three-part rollout: relicensing `aivyx-coder` itself.
Part 2 (the 9 shared subsystem crates `aivyx-coder` depends on) is separate,
later work. The operator's decision (confirmed during Part 0) is uniform
BUSL-1.1 across `aivyx-coder` and every shared crate, no exceptions —
competitive-moat/IP-protection motivated, not monetization-motivated.

## Grounding

Read and verified directly, not assumed:

- **`aivyx-coder`'s current legal state**: `LICENSE` + `LICENSE-MIT` +
  `LICENSE-APACHE` (the classic Rust dual-license pattern — three files,
  not `aivyx-pa`'s single-`LICENSE`-plus-`LICENSES/MIT.txt` shape).
  `Cargo.toml`'s `[workspace.package] license = "MIT OR Apache-2.0"`.
  `CONTRIBUTING.md` is 27 lines with no CLA/DCO gate at all. No
  `TRADEMARK.md`, `COMMERCIAL.md`, `CLA.md`, or `deny.toml` exist today.
  README.md carries a license badge
  (`![License: Apache-2.0 OR MIT](...)`) linking to `LICENSE`.
- **The `cargo deny check licenses` gate is clean, verified directly**: ran
  `cargo-deny --all-features check licenses` against `aivyx-coder`'s real
  dependency graph using `aivyx-pa`'s own established, documented allow-list
  (`aivyx-pa/deny.toml`'s `[licenses]` section, copied verbatim) —
  **`licenses ok`**, zero rejections. Unlike `aivyx-pa`'s own CR.1 audit,
  which found a genuine GPL-3.0 blocker (`piper1-rs-sys`), `aivyx-coder`'s
  graph has no GPL/AGPL/SSPL/LGPL-only dependency anywhere. The only
  weak-copyleft license present is `MPL-2.0`, from the exact same
  transitive source `aivyx-pa` already documented and allow-listed
  (the servo CSS stack — `cssparser`/`dtoa-short`/`selectors`, pulled via
  `scraper`) — file-level copyleft that never touches files `aivyx-coder`
  itself modifies, so it's compatible with shipping under BUSL-1.1 for the
  same reason `aivyx-pa`'s own allow-list already documents.
- **`aivyx-pa`'s own shipped relicense** (`aivyx-pa/docs/LICENSING.md`,
  `aivyx-pa/LICENSE`, `aivyx-pa/Cargo.toml`): `license = "BUSL-1.1"` is
  written directly into `Cargo.toml` even though it isn't a valid SPDX
  identifier — uncontroversial here since none of these crates publish to
  crates.io (git-dependency-only). `publish = false` is set
  workspace-wide so `cargo-deny`'s `private = { ignore = true }` skips
  `aivyx-pa`'s own first-party BUSL crates while still catching a
  third-party copyleft/BUSL dependency. `LICENSE` is the filled canonical
  BUSL-1.1 text; `LICENSES/MIT.txt` holds the Change License text
  verbatim.
- **The CLA-supersession follow-up Part 0 flagged is resolved** (separate
  small change, already shipped, `aivyx-pa` commit `5ffea26e`,
  2026-09-24): `aivyx-pa`'s own project-scoped `CLA.md` now points to the
  org-wide `aivyx-ecosystem/docs/legal/CLA.md` instead of duplicating it.
  `aivyx-coder`'s own new `CLA.md` (this Part) is a **physical copy** of
  the org-wide template (per Part 0's Decision 2), not a pointer —
  matching how every other target repo adopts it.
- **Part 0's binding decisions**, already made and not reopened here:
  uniform BUSL-1.1, no per-crate exceptions (Decision 3); `aivyx-coder`
  gains a new `TRADEMARK.md` it doesn't have today (Decision 4);
  `Licensor`/Additional-Use-Grant/`Change-Date`(4yr per-release)/
  `Change-License`(MIT) text is byte-identical to `aivyx-pa`'s own and the
  canonical template, only `LICENSED_WORK` varies (Decision 2); the
  docs-correction standard — every "open source"/"MIT"/"Apache-2.0"
  self-claim about a relicensed repo's own code gets corrected to
  "source-available under BUSL-1.1" (Decision 5) — applies to
  `aivyx-coder`'s own README/CLAUDE.md here; the outer workspace-root
  `CLAUDE.md` and `aivyx-ecosystem`'s own README/ROADMAP correction stays
  tracked for Part 2, not duplicated here.

## Decisions

**1. `deny.toml` ships as a permanent gate, not a one-time check.**
`aivyx-pa`'s exact `[licenses]` allow-list (copied verbatim, including its
documented rationale comments) plus `[graph] all-features = true` and
`private = { ignore = true }`. This is Part 1's first concrete deliverable —
codifies the audit finding above as a durable, reviewable file so a future
copyleft dependency fails the gate loudly instead of silently landing.

**2. LICENSE file structure matches `aivyx-pa`'s exactly**: `LICENSE`
(filled BUSL-1.1, `LICENSED_WORK` = "Aivyx Coder, the first version released
under this License and all later versions") + `LICENSES/MIT.txt` (Change
License text). `LICENSE-MIT` and `LICENSE-APACHE` are deleted — confirmed
directly with the operator (not a default assumption) — since the Change
License is MIT-only going forward, Apache-2.0 stops being an option and
keeping a dead Apache license file would be confusing paperwork with no
legal benefit.

**3. `CLA.md`, `COMMERCIAL.md`, `TRADEMARK.md` are physical copies** of the
canonical `aivyx-ecosystem/docs/legal/` templates, with only the documented
placeholders filled: `{{LICENSED_WORK_NAME}}` / `{{PROJECT_NAME}}` /
`{{PRODUCT_NAME}}` → "Aivyx Coder", `{{CLA_PATH}}` → `CLA.md` (this repo's
own local copy, not the org-wide URL — matching Part 0's own disposition
table: every target repo gets its own physically-copied `CLA.md`).

**4. `CONTRIBUTING.md` gets the CLA-gate section + DCO text spliced in**
from `CONTRIBUTING-cla-section.md`'s two template blocks, placed near the
top (matching `aivyx-pa/CONTRIBUTING.md`'s own §1 placement) and near the
end (DCO text) respectively. The existing "Before you open a PR" and
"Building & testing" sections stay as-is; only the "## License" section at
the bottom gets replaced (it currently says contributions are dual
Apache-2.0/MIT-licensed, which becomes wrong the moment `LICENSE` changes).

**5. `Cargo.toml`: `license = "BUSL-1.1"` plus `publish = false`
workspace-wide.** The `publish = false` addition matches `aivyx-pa`'s own
CR.2 exactly and exists for the same reason: it lets `cargo-deny`'s
`private = { ignore = true }` correctly skip `aivyx-coder`'s own first-party
crates (all now BUSL, which would otherwise fail the same allow-list that's
supposed to catch *third-party* copyleft/BUSL dependencies) without
weakening the gate against a real third-party violation.

**6. `README.md` docs-correction**: the existing license badge
(`![License: Apache-2.0 OR MIT]`) is replaced with a BUSL-1.1 equivalent
linking to `LICENSE`; any other "open source"/"MIT"/"Apache-2.0"
self-description of `aivyx-coder`'s own code anywhere in `README.md` or
`CLAUDE.md` is corrected to "source-available under BUSL-1.1," matching
`aivyx-pa`'s own CR.5 wording. Third-party mentions (e.g. "Ollama is MIT")
are left alone.

**7. Version bump + tag + release**: the operator confirmed Part 1 ends
with a real tagged release, matching `aivyx-pa`'s CR.6 precedent (so the
Change Date — 4 years from release — is concrete, not floating). Current
version is `0.1.0` (one existing tag, `v0.1.0`). Bump to **`0.2.0`**,
changelog/release-notes leading with the license change (matching
`aivyx-pa`'s own "license change leads the notes" precedent), then run the
repo's existing `.github/workflows/release.yml` for the tagged release.

## What this spec does not decide

- Any change to the 9 shared subsystem crates `aivyx-coder` depends on via
  git — that's Part 2, entirely separate. `aivyx-coder`'s `Cargo.toml`
  dependency pins on `aivyx-confine`/`aivyx-checkpoint`/`aivyx-kvcache`/
  `aivyx-recall`/`aivyx-injection-guard`/`aivyx-skills` are untouched here;
  those crates remain MIT/Apache-2.0 until Part 2 relicenses them, which is
  not a compatibility problem (BUSL-licensed code may depend on MIT/Apache
  dependencies without issue — only copyleft dependencies are the concern
  the `deny.toml` gate exists to catch).
- The outer workspace-root `CLAUDE.md` (`/home/julian/Projects/Rust/CLAUDE.md`)
  or `aivyx-ecosystem`'s own README/ROADMAP correction — tracked for Part 2
  per Part 0's Decision 5, not duplicated here.
- Pricing or commercial terms beyond the placeholder contact-based model
  `COMMERCIAL.md`'s template already carries.
- Any change to `aivyx-pa` beyond the already-shipped CLA-supersession
  commit (`5ffea26e`) — that follow-up is resolved, not reopened here.
- A CLA-signing automation mechanism beyond `git commit -s` — reuses the
  same low-friction convention `aivyx-pa` and Part 0 already established.
