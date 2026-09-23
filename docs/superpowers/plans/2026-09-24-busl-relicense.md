# BUSL-1.1 Relicense (Part 1: aivyx-coder) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Relicense `aivyx-coder` from `MIT OR Apache-2.0` to BUSL-1.1, reusing the canonical legal templates `aivyx-ecosystem`'s Part 0 groundwork already shipped, and cut a tagged `v0.2.0` release with the change.

**Architecture:** Six sequential file-level tasks: a permanent `cargo-deny` license gate, the LICENSE file swap + `Cargo.toml` license/publish fields, three physically-copied legal documents (CLA/COMMERCIAL/TRADEMARK), a `CONTRIBUTING.md` restructure, a docs-correction pass on `README.md`, and finally the version bump + release prep. Each task is independently testable and commits on its own.

**Tech Stack:** Rust/Cargo workspace, `cargo-deny` for license auditing, Markdown legal documents, GitHub Actions (`release.yml`, triggered by pushing a `v*` tag).

## Global Constraints

- `Licensor` / Additional Use Grant / `Change Date` (four years, per-release) / `Change License` (MIT) text is **byte-identical** to the canonical template at `aivyx-ecosystem/docs/legal/busl-license-template.md` and to `aivyx-pa`'s own shipped `LICENSE` — never edit it beyond filling `{{LICENSED_WORK_NAME}}`.
- Every placeholder (`{{LICENSED_WORK_NAME}}`, `{{PROJECT_NAME}}`, `{{PRODUCT_NAME}}`) fills to **"Aivyx Coder"**. Path placeholders (`{{LICENSE_PATH}}`, `{{COMMERCIAL_PATH}}`, `{{CLA_PATH}}`) fill to **`LICENSE`**, **`COMMERCIAL.md`**, **`CLA.md`** respectively — this repo's own root-level copies, not the `aivyx-ecosystem` originals.
- The `deny.toml` license allow-list must exactly match `aivyx-pa`'s own established list (`aivyx-pa/deny.toml`'s `[licenses]` section) — already verified directly against `aivyx-coder`'s real all-features dependency graph (`cargo deny --all-features check licenses` → `licenses ok`, zero rejections, no GPL/AGPL/copyleft-only dependency anywhere).
- Bumping the workspace version requires updating **both** `Cargo.toml`'s `[workspace.package] version` **and** every hardcoded `version = "0.1.0"` path-dependency pin across the 7 crate manifests that have them — verified empirically that skipping the pins breaks `cargo check` (`failed to select a version for the requirement 'aivyx-acp = "^0.1.0"'`).
- No task pushes a git tag or triggers the release workflow. The final task creates the tag **locally only** — pushing it (which fires `.github/workflows/release.yml`) is a separate, explicit step outside this plan's automation.
- Every step that changes a file ends with the file in a buildable/valid state — run `cargo check --workspace` (or the task's own narrower check) before committing, not just at the very end.

---

### Task 1: Permanent license gate (`deny.toml`)

**Files:**
- Create: `deny.toml`

**Interfaces:**
- Produces: `deny.toml` at repo root — later tasks (2, 6) each re-run `cargo deny --all-features check licenses` against it to confirm the gate still passes after their own changes.

- [ ] **Step 1: Confirm `cargo-deny` is available**

Run: `cargo deny --version`

If it errors with "no such command," install it first:

```sh
cargo install cargo-deny --locked
```

- [ ] **Step 2: Write `deny.toml`**

```toml
# cargo-deny configuration — dependency license policy.
#
# Run: `cargo deny check licenses`
#
# aivyx-coder relicenses MIT OR Apache-2.0 -> BUSL-1.1 (see LICENSE). For
# that to be lawful, NO dependency may impose copyleft (GPL/LGPL-only/
# AGPL/MPL/SSPL/EPL/CDDL) on the combined work. This allow-list is
# therefore permissive-only and deliberately omits every copyleft
# license: cargo-deny rejects anything not listed, so a future copyleft
# dep fails the gate loudly instead of silently poisoning the relicense.
#
# The list matches aivyx-pa's own established allow-list exactly (that
# repo went through this same relicense first, and its own audit found a
# real GPL-3.0 blocker before settling on this list) — reused rather
# than redrafted so both repos apply the identical policy.

[graph]
all-features = true

[licenses]
version = 2
# Skip aivyx-coder's OWN workspace crates: they relicense to BUSL-1.1
# (this gate's whole purpose), so they must not be measured against the
# dependency allow-list below. Scoped to first-party crates marked
# publish = false (see [workspace.package], set in Task 2 of this
# plan) -- a THIRD-PARTY crate under BUSL/any copyleft still fails
# loudly, which is the point.
private = { ignore = true }
allow = [
    "MIT",
    "MIT-0",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "ISC",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "Zlib",
    "BSL-1.0",
    "0BSD",
    "Unlicense",
    "WTFPL",
    "CC0-1.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "CDLA-Permissive-2.0",
    "NCSA",
    # MPL-2.0 is *file-level* (weak) copyleft: it never relicenses the
    # larger combined work -- the only obligation is to share
    # modifications to the MPL-licensed files themselves, which we never
    # touch (they are upstream crates: the servo CSS stack --
    # cssparser/selectors/dtoa-short -- pulled transitively via the
    # `scraper` crate). Fully compatible with shipping aivyx-coder under
    # BUSL-1.1. Verified directly: `cargo deny check licenses
    # --all-features` is green with this allow-list and no exceptions.
    "MPL-2.0",
]

# No per-crate license exceptions. `cargo deny check licenses`
# (all-features) is green with no exceptions -- verified directly
# against this exact allow-list before this file was committed. Any
# future copyleft dep now fails the gate loudly.
```

- [ ] **Step 3: Run the gate and confirm it passes**

Run: `cargo deny --all-features check licenses`

Expected: the command prints `licenses ok` near the end (two
`warning[license-not-encountered]` lines for `WTFPL` and
`Unicode-DFS-2016` are expected and harmless — those licenses aren't
present in this graph, only kept in the allow-list for consistency with
`aivyx-pa`'s own file). Exit code `0`.

- [ ] **Step 4: Commit**

```bash
git add deny.toml
git commit -m "Add cargo-deny license gate ahead of BUSL-1.1 relicense

Codifies a directly-verified finding as a durable, reviewable file: the
all-features dependency graph is clean against aivyx-pa's own
established permissive allow-list, no GPL/AGPL/copyleft-only dependency
anywhere. A future copyleft dependency now fails this gate loudly
instead of landing silently."
```

---

### Task 2: LICENSE swap + `Cargo.toml` license/publish fields

**Files:**
- Modify: `LICENSE` (rewritten in place)
- Create: `LICENSES/MIT.txt`
- Delete: `LICENSE-MIT`
- Delete: `LICENSE-APACHE`
- Modify: `Cargo.toml` (root workspace manifest)
- Modify: `crates/aivyx-acp/Cargo.toml`, `crates/aivyx/Cargo.toml`, `crates/aivyx-config/Cargo.toml`, `crates/aivyx-core/Cargo.toml`, `crates/aivyx-llm/Cargo.toml`, `crates/aivyx-mcp-server/Cargo.toml`, `crates/aivyx-repomap/Cargo.toml`, `crates/aivyx-sandbox/Cargo.toml`, `crates/aivyx-team/Cargo.toml`, `crates/aivyx-tools/Cargo.toml`, `crates/aivyx-tui/Cargo.toml`, `crates/aivyx-types/Cargo.toml` (all 12 crate manifests — each gets one new line)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: `LICENSE` (BUSL-1.1 text), `LICENSES/MIT.txt` — Task 3's `TRADEMARK.md`/`COMMERCIAL.md` link to `LICENSE`; Task 4's `CONTRIBUTING.md` links to `LICENSE`. `Cargo.toml`'s `license = "BUSL-1.1"` and `publish = false` — later tasks don't reference these directly, but Task 1's `deny.toml` `private = { ignore = true }` only takes effect once this task lands.

- [ ] **Step 1: Rewrite `LICENSE`**

Replace the entire contents of `LICENSE` with:

```text
Business Source License 1.1

Parameters

Licensor:             Julian (Aivyx) / the Aivyx-Agent project.

Licensed Work:        Aivyx Coder, the first version released under
                      this License and all later versions.
                      The Licensed Work is (c) 2026 Julian (Aivyx).

Additional Use Grant: You may use, copy, modify, and create derivative works of
                      the Licensed Work for personal, individual, educational,
                      research, evaluation, and other non-commercial purposes.

                      "Non-commercial" means use that is not primarily intended
                      for or directed toward commercial advantage or monetary
                      compensation, including use by an individual for personal
                      projects and use by a registered non-profit or accredited
                      educational institution.

                      Any other use -- including any use by or on behalf of a
                      for-profit entity, any use in production in connection with
                      a commercial product or service, and any use that generates
                      revenue -- requires a commercial license from the Licensor.

Change Date:          Four years from the date the Licensed Work is published.
                      This License applies separately for each version of the
                      Licensed Work, and the Change Date may vary for each.

Change License:       MIT License (the terms preserved in this repository at
                      LICENSES/MIT.txt).

For information about alternative licensing arrangements for the Licensed Work,
please see COMMERCIAL.md or contact the Licensor.

Notice

The Business Source License (this document, or the "License") is not an Open
Source license. However, the Licensed Work will eventually be made available
under an Open Source License, as stated in this License.

License text copyright (c) 2017 MariaDB Corporation Ab, All Rights Reserved.
"Business Source License" is a trademark of MariaDB Corporation Ab.

-----------------------------------------------------------------------------

Business Source License 1.1

Terms

The Licensor hereby grants you the right to copy, modify, create derivative
works, redistribute, and make non-production use of the Licensed Work. The
Licensor may make an Additional Use Grant, above, permitting limited
production use.

Effective on the Change Date, or the fourth anniversary of the first publicly
available distribution of a specific version of the Licensed Work under this
License, whichever comes first, the Licensor hereby grants you rights under
the terms of the Change License, and the rights granted in the paragraph
above terminate.

If your use of the Licensed Work does not comply with the requirements
currently in effect as described in this License, you must purchase a
commercial license from the Licensor, its affiliated entities, or authorized
resellers, or you must refrain from using the Licensed Work.

All copies of the original and modified Licensed Work, and derivative works
of the Licensed Work, are subject to this License. This License applies
separately for each version of the Licensed Work and the Change Date may vary
for each version of the Licensed Work released by Licensor.

You must conspicuously display this License on each original or modified copy
of the Licensed Work. If you receive the Licensed Work in original or
modified form from a third party, the terms and conditions set forth in this
License apply to your use of that work.

Any use of the Licensed Work in violation of this License will automatically
terminate your rights under this License for the current and all other
versions of the Licensed Work.

This License does not grant you any right in any trademark or logo of
Licensor or its affiliates (provided that you may use a trademark or logo of
Licensor as expressly required by this License).

TO THE EXTENT PERMITTED BY APPLICABLE LAW, THE LICENSED WORK IS PROVIDED ON
AN "AS IS" BASIS. LICENSOR HEREBY DISCLAIMS ALL WARRANTIES AND CONDITIONS,
EXPRESS OR IMPLIED, INCLUDING (WITHOUT LIMITATION) WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE, NON-INFRINGEMENT, AND
TITLE.

MariaDB hereby grants you permission to use this License's text to license
your works, and to refer to it using the trademark "Business Source License",
as long as you comply with the Covenants of Licensor below.

Covenants of Licensor

In consideration of the right to use this License's text and the "Business
Source License" name and trademark, Licensor covenants to MariaDB, and to all
other recipients of the licensed work to be provided by Licensor:

1. To specify as the Change License the GPL Version 2.0 or any later version,
   or a license that is compatible with GPL Version 2.0 or a later version,
   where "compatible" means that software provided under the Change License can
   be included in a program with software provided under GPL Version 2.0 or a
   later version. Licensor may specify additional Change Licenses without
   limitation.

2. To either: (a) specify an additional grant of rights to use that does not
   impose any additional restriction on the right granted in this License, as
   the Additional Use Grant; or (b) insert the text "None".

3. To specify a Change Date.

4. Not to modify this License in any other way.
```

- [ ] **Step 2: Create `LICENSES/MIT.txt`**

```text
MIT License

Copyright (c) 2026 Julian (Aivyx)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

- [ ] **Step 3: Delete the old dual-license files**

```bash
git rm LICENSE-MIT LICENSE-APACHE
```

- [ ] **Step 4: Update the root `Cargo.toml`**

In `Cargo.toml`, in the `[workspace.package]` block, change:

```toml
license = "MIT OR Apache-2.0"
```

to:

```toml
license = "BUSL-1.1"
publish = false
```

(This isn't a valid SPDX identifier, which is fine — `aivyx-pa` did the same in its own `Cargo.toml`; none of these crates publish to crates.io.)

- [ ] **Step 5: Add `publish.workspace = true` to all 12 crate manifests**

Every crate's `[package]` block currently ends with `license.workspace = true` and nothing about `publish`. Add one line right after `license.workspace = true` in each of the 12 files:

```bash
for f in crates/aivyx-acp/Cargo.toml crates/aivyx/Cargo.toml crates/aivyx-config/Cargo.toml crates/aivyx-core/Cargo.toml crates/aivyx-llm/Cargo.toml crates/aivyx-mcp-server/Cargo.toml crates/aivyx-repomap/Cargo.toml crates/aivyx-sandbox/Cargo.toml crates/aivyx-team/Cargo.toml crates/aivyx-tools/Cargo.toml crates/aivyx-tui/Cargo.toml crates/aivyx-types/Cargo.toml; do
  sed -i '/^license\.workspace = true$/a publish.workspace = true' "$f"
done
```

Verify all 12 picked it up:

```bash
grep -l "publish.workspace = true" crates/*/Cargo.toml | wc -l
```

Expected: `12`.

- [ ] **Step 6: Verify the workspace still builds**

Run: `cargo check --workspace`

Expected: succeeds with no errors (warnings from existing code are fine; this step only needs to prove the manifest edits didn't break dependency resolution).

- [ ] **Step 7: Re-run the license gate and confirm `private.ignore` now applies**

Run: `cargo deny --all-features check licenses`

Expected: still `licenses ok`. This confirms the mechanism from Task 1 actually works end-to-end now that `publish = false` is set: `aivyx-coder`'s own crates are now `BUSL-1.1` (not in the allow-list), but `private = { ignore = true }` correctly skips them, so the gate stays green — while a real third-party BUSL/copyleft dependency would still fail it.

- [ ] **Step 8: Commit**

```bash
git add LICENSE LICENSES/MIT.txt Cargo.toml crates/*/Cargo.toml
git commit -m "Relicense aivyx-coder to BUSL-1.1

LICENSE is now the filled canonical BUSL-1.1 text (Change License =
MIT, preserved verbatim at LICENSES/MIT.txt; four-year per-release
Change Date). LICENSE-MIT and LICENSE-APACHE are removed -- Apache-2.0
stops being an option going forward since the Change License is
MIT-only. Cargo.toml: license = \"BUSL-1.1\", publish = false
workspace-wide (every crate now inherits publish.workspace = true) so
cargo-deny's private.ignore correctly skips our own now-BUSL crates
while still catching a real third-party copyleft/BUSL dependency --
verified directly, the gate from the previous commit stays green."
```

---

### Task 3: Physically-copied legal documents (`CLA.md`, `COMMERCIAL.md`, `TRADEMARK.md`)

**Files:**
- Create: `CLA.md`
- Create: `COMMERCIAL.md`
- Create: `TRADEMARK.md`

**Interfaces:**
- Consumes: `LICENSE` (Task 2) — `TRADEMARK.md` and `COMMERCIAL.md` link to it.
- Produces: `CLA.md`, `COMMERCIAL.md`, `TRADEMARK.md` at repo root — Task 4's `CONTRIBUTING.md` links to all three.

- [ ] **Step 1: Create `CLA.md`**

Copy the full text below verbatim (this is `aivyx-ecosystem/docs/legal/CLA.md` — the org-wide CLA — with no placeholders; it needs no repo-specific filling since it's already written to cover any Aivyx-Agent repository):

```markdown
# Aivyx-Agent Contributor License Agreement

**Version 1.0**

Thank you for contributing to an Aivyx-Agent project. This Contributor
License Agreement (the "Agreement") sets out the terms under which You
provide Contributions to the Licensor. It exists so that Aivyx-Agent
software can be offered under its dual model — source-available under
the Business Source License 1.1 for free personal/non-commercial use,
and under separate commercial licenses for commercial use (see each
repository's own `LICENSE` and `COMMERCIAL.md`).

This Agreement covers Contributions to **any current or future
repository published by the Licensor under the Aivyx-Agent GitHub
organization** ("the Project") — not just the repository you are
contributing to today. You accept it the same way in every repository
(see "How to accept" below) — per contribution, via a `Signed-off-by`
trailer — and its terms apply org-wide: a right you grant once, in
substance, even though the sign-off mechanic is checked per commit, per
repository.

**You keep the copyright in Your Contributions.** This Agreement is a
*license grant*, not an assignment. It does not stop You from using
Your Contributions for any other purpose.

By submitting a Contribution to the Project (including by adding a
`Signed-off-by` trailer to a commit, as described in that repository's
own `CONTRIBUTING.md`), You agree to the following terms for that and
all future Contributions to the Project.

## 1. Definitions

- **"Licensor"** means Julian (Aivyx) / the Aivyx-Agent project, the
  steward of the Project.
- **"Project"** means any current or future software repository
  published by the Licensor under the Aivyx-Agent GitHub organization.
- **"You"** (or **"Your"**) means the individual or legal entity
  agreeing to this Agreement. If You are agreeing on behalf of a legal
  entity, "You" includes that entity and its affiliates, and You
  represent that You are authorized to bind it.
- **"Contribution"** means any original work of authorship — including
  any modifications or additions to existing work — that You
  intentionally submit to the Licensor for inclusion in, or
  documentation of, the Project. "Submit" means any form of electronic,
  verbal, or written communication sent to the Licensor or its
  representatives (for example, pull requests, patches, issues, and
  code review comments), excluding communication conspicuously marked
  or otherwise designated in writing by You as "Not a Contribution."

## 2. Grant of Copyright License

Subject to the terms of this Agreement, You grant to the Licensor and to
recipients of software distributed by the Licensor a **perpetual,
worldwide, non-exclusive, royalty-free, irrevocable** copyright license
to reproduce, prepare derivative works of, publicly display, publicly
perform, sublicense, and distribute Your Contributions and such
derivative works.

In addition, and without limiting the foregoing, You grant the Licensor
the right to **license and sublicense Your Contributions, and
derivative works of them, under any license terms of the Licensor's
choosing — including the Business Source License 1.1, the MIT License,
and separate commercial (paid) license terms — and to change those
terms for future versions.** This is the right that permits Aivyx-Agent
software to be offered for free for non-commercial use and under paid
commercial licenses at the same time.

## 3. Grant of Patent License

Subject to the terms of this Agreement, You grant to the Licensor and to
recipients of software distributed by the Licensor a perpetual,
worldwide, non-exclusive, royalty-free, irrevocable (except as stated in
this section) patent license to make, have made, use, offer to sell,
sell, import, and otherwise transfer the Project, where such license
applies only to those patent claims licensable by You that are
necessarily infringed by Your Contribution alone or by combination of
Your Contribution with the Project.

If any entity institutes patent litigation against You or any other
entity (including a cross-claim or counterclaim in a lawsuit) alleging
that Your Contribution, or the Project to which You have contributed,
constitutes direct or contributory patent infringement, then any patent
licenses granted to that entity under this Agreement for that
Contribution or work shall terminate as of the date such litigation is
filed.

## 4. Your Representations

You represent that:

1. You are **legally entitled to grant the above licenses.** If Your
   employer(s) have rights to intellectual property that You create
   that includes Your Contributions, You represent that You have
   received permission to make Contributions on behalf of that
   employer, that Your employer has waived such rights for Your
   Contributions to the Licensor, or that Your employer has executed a
   separate agreement with the Licensor.
2. Each of Your Contributions is **Your original creation** (see
   Section 5 for submissions that are not Your original creation).
3. Your Contributions include complete details of any third-party
   license or other restriction (including related patents and
   trademarks) of which You are personally aware and which are
   associated with any part of Your Contributions.

## 5. Third-Party Works

Should You wish to submit work that is not Your original creation, You
may submit it to the Licensor separately from any Contribution,
identifying the complete details of its source and of any license or
other restriction (including related patents, trademarks, and license
agreements) of which You are personally aware, and conspicuously
marking the work as "Submitted on behalf of a third party: [named
here]."

## 6. Support and Warranties

You are not expected to provide support for Your Contributions, except
to the extent You desire to provide support. Unless required by
applicable law or agreed to in writing, You provide Your Contributions
on an **"AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND**,
either express or implied, including, without limitation, any
warranties or conditions of TITLE, NON-INFRINGEMENT, MERCHANTABILITY, or
FITNESS FOR A PARTICULAR PURPOSE.

## 7. No Obligation

The Licensor is not obligated to use, include, or distribute Your
Contributions. The decision to include any Contribution in the Project
is entirely at the Licensor's discretion.

## 8. Miscellaneous

You agree to notify the Licensor of any facts or circumstances of which
You become aware that would make these representations inaccurate in
any respect. This Agreement is the entire agreement between You and the
Licensor regarding Your Contributions, and supersedes any prior
agreement on that subject.

---

## How to accept

You accept this Agreement for a contribution by adding a
`Signed-off-by` trailer to each commit in your pull request:

```sh
git commit -s -m "your message"
```

See that repository's own `CONTRIBUTING.md` for full instructions. The
same sign-off also certifies the Developer Certificate of Origin.
```

- [ ] **Step 2: Create `COMMERCIAL.md`**

Copy the full text below verbatim (this is `aivyx-ecosystem/docs/legal/COMMERCIAL.md` — already written to apply to any BUSL-1.1 repo in the org, no placeholders):

```markdown
# Commercial Licensing

Aivyx-Agent software is **source-available** under the Business Source
License 1.1 (BUSL-1.1). It is **free for personal and non-commercial
use**, and requires a **paid commercial license for business or
production use**. This page explains, in plain English, which side of
that line you are on and how to get a license if you need one. It
applies to every BUSL-1.1 repository in the Aivyx-Agent organization —
see each repository's own `LICENSE` for its exact terms.

> The authoritative terms are in each repository's own `LICENSE`; the
> reasoning behind the model is in this ecosystem's
> [`LICENSING.md`](https://github.com/Aivyx-Agent/aivyx-ecosystem/blob/main/LICENSING.md)
> and, for `aivyx-pa` specifically, `aivyx-pa/docs/LICENSING.md`. Where
> this page and a repository's `LICENSE` ever appear to differ, that
> repository's `LICENSE` governs.

## Free — no license needed

You may use, copy, modify, and build on Aivyx-Agent software **at no
cost** for **personal and non-commercial** purposes. That includes:

- An **individual** using the software for personal purposes.
- **Learning, evaluation, experimentation, and research.**
- **Hobby and personal projects** that are not run for commercial
  advantage.
- Use by a **registered non-profit** or an **accredited educational
  institution**.

If that describes you, you are done — enjoy the software, and nothing
below applies.

## Paid — a commercial license is required

You need a commercial license for any use that is **primarily intended
for, or directed toward, commercial advantage or monetary
compensation.** Concretely, this includes:

- Any use **by or on behalf of a for-profit company** — including
  internal use by employees or contractors (there is no "internal use
  is free" carve-out).
- Running the software **in production** as part of, or in support of,
  a **commercial product or service**.
- Any use that **generates revenue**, directly or indirectly.
- **Offering the software to third parties** — hosted, embedded,
  resold, or as a service.

In short: **if a business depends on it, the business buys a license.**
This is true whether you run the stock build or your own modified fork.

### Quick check

| Your situation | What you need |
|---|---|
| Individual, personal use | **Free** |
| Student / researcher / evaluating it | **Free** |
| Registered non-profit / accredited school | **Free** |
| A for-profit company, even just internally | **Commercial license** |
| Production use behind a paid product or service | **Commercial license** |
| Hosting or reselling the software to customers | **Commercial license** |

Not sure which bucket you fall in? **Ask** (below) — we would rather
answer than have you guess.

## It becomes MIT eventually

BUSL-1.1 is not permanent. **Four years after a given version is
published, that version automatically converts to the [MIT
License](LICENSES/MIT.txt)** — fully open source, no restrictions, no
payment. Each release carries its
own four-year clock. The commercial license covers the period before a
version's conversion (and the convenience, support, and assurances that
come with it).

## How to get a commercial license

Email **aivyx@aivyx-studio.com** with:

- **Who you are** — company / organization name.
- **Which repository/repositories** you intend to use.
- **How you intend to use it** — internal tooling, embedded in a
  product, hosted service, etc.
- **Rough scale** — team size, number of deployments, or end users, if
  known.

We will reply with terms and next steps.

## Pricing

> **Pricing is being finalized.** Commercial terms are currently quoted
> per-engagement based on use and scale — email
> **aivyx@aivyx-studio.com** for a quote. A standard published price
> list will land here as the commercial offering matures.

## A note on trademark

A commercial *code* license is separate from **Aivyx product names and
branding**, which are trademarked. Permission to use software under
either the free grant or a commercial license does **not** grant rights
to any Aivyx name or logo — see that repository's `TRADEMARK.md`, where
one is present, for brand-usage rules.
```

- [ ] **Step 3: Create `TRADEMARK.md`**

Copy the text below, filling `{{PRODUCT_NAME}}` → `Aivyx Coder` (this repo ships a consumer-facing product name, so it gets a `TRADEMARK.md` per the design spec's Decision 4 — the 9 shared subsystem crates do not):

```markdown
# Trademark Notice

The code in this repository is **source-available under BUSL-1.1**
(free for personal/non-commercial use; a [commercial
license](COMMERCIAL.md) for business or production use — see
[`LICENSE`](LICENSE)). The "Aivyx" name (the organization/brand) and the
"Aivyx Coder" name (the product built on this code), along with
their logos and associated branding, are **not** covered by that
license — they are trademarks, and a code license (free or commercial)
grants no rights to them.

## What you can do

- Fork the code
- Modify it, rebuild it, redistribute it (for personal/non-commercial
  use under the BUSL grant, or under a commercial license for
  commercial use)
- Use it commercially **with a [commercial license](COMMERCIAL.md)**
- Contribute upstream (see [`CONTRIBUTING.md`](CONTRIBUTING.md))

## What you cannot do

- Call your fork "Aivyx", "Aivyx Coder", or any confusingly
  similar name
- Use the Aivyx or Aivyx Coder logo or marketing materials without
  permission
- Distribute your fork through official Aivyx channels (aivyx.ai
  domains, official app store listings, branded installers)
- Claim affiliation with the Aivyx project or the Aivyx Coder
  product if there is none

If you fork, please rename. Precedents: Code-OSS vs. VSCode, Chromium
vs. Chrome, Firefox vs. Iceweasel.

## Contact

For trademark licensing inquiries: julian@aivyx-studio.com
```

- [ ] **Step 4: Verify no leftover placeholders**

Run: `grep -n '{{' CLA.md COMMERCIAL.md TRADEMARK.md`

Expected: no output (empty match — confirms every `{{...}}` placeholder was filled).

- [ ] **Step 5: Commit**

```bash
git add CLA.md COMMERCIAL.md TRADEMARK.md
git commit -m "Add CLA.md, COMMERCIAL.md, TRADEMARK.md

Physical copies of the canonical org-wide templates from
aivyx-ecosystem/docs/legal/ (Part 0 groundwork), with only the
documented placeholders filled -- Aivyx Coder is the product name
throughout. TRADEMARK.md is new: aivyx-coder ships a consumer-facing
product name, so per the design spec's Decision 4 it gets one (the 9
shared subsystem crates, infrastructure rather than products, do not)."
```

---

### Task 4: `CONTRIBUTING.md` restructure

**Files:**
- Modify: `CONTRIBUTING.md` (full rewrite)

**Interfaces:**
- Consumes: `LICENSE`, `COMMERCIAL.md`, `CLA.md` (Tasks 2-3) — this task's new CLA-gate section links to all three.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Replace the full contents of `CONTRIBUTING.md`**

The existing file has three sections: "Before you open a PR", "Building
& testing", and "License" (the last one is now wrong — it describes
dual Apache-2.0/MIT terms). The new version keeps the first two
unchanged, inserts a new CLA-gate section right after the intro
(matching where `aivyx-pa/CONTRIBUTING.md` places its own equivalent
section — that file numbers its sections `## 1.`, `## 2.`, etc., but
this file has never used numbered headings, so the new section stays
unnumbered too, consistent with this file's own existing style), and
replaces the old "License" section with a new "Developer Certificate of
Origin" section at the end:

```markdown
# Contributing to aivyx-coder

Thanks for your interest in contributing.

## The Contributor License Agreement (required)

Aivyx Coder is **source-available under [BUSL-1.1](LICENSE)**
and is offered under a dual model — **free for personal/non-commercial
use, paid for commercial use** (see [`COMMERCIAL.md`](COMMERCIAL.md)).
For that model to be lawful, the project must hold the right to license
**all** of the code — including your contributions — under both the
BUSL-1.1 terms and separate commercial terms.

A plain "inbound = outbound" contribution does **not** give the project
the right to commercially sublicense your code. So, **before any
contribution can be merged, you must agree to the [Contributor License
Agreement (`CLA.md`)](CLA.md).** In short, the CLA: you keep
your copyright, and you grant the Licensor a broad, irrevocable license
to use, relicense, and **commercially sublicense** your contributions.
This agreement covers contributions to any Aivyx-Agent repository, not
just this one — read [`CLA.md`](CLA.md) for the exact terms,
it is short.

**How you agree:** every commit in your pull request must carry a
`Signed-off-by` trailer matching the author, added automatically by
committing with `-s`:

```sh
git commit -s -m "your message"
```

By signing off you certify the [Developer Certificate of
Origin](#developer-certificate-of-origin) **and** accept the
[CLA](CLA.md) for that contribution. A maintainer cannot merge a
PR whose commits are not signed off. If you are contributing on behalf
of an employer, make sure you have their permission first (the CLA
covers this).

> **Why this exists:** without it, the relicense and the commercial
> offering could not legally cover contributed lines. This gate must
> precede any external PR — see the ecosystem's own
> [`LICENSING.md`](https://github.com/Aivyx-Agent/aivyx-ecosystem/blob/main/LICENSING.md).

## Before you open a PR

- For anything beyond a small fix, please open an issue first to describe
  the shape of the change so we can agree on the approach.
- Keep changes focused — a PR should do one thing.

## Building & testing

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets

# Single crate / single test
cargo test -p aivyx-core
cargo test -p aivyx-core some_test_name
```

## Developer Certificate of Origin

By making a contribution to this project, you certify the
[Developer Certificate of Origin 1.1](https://developercertificate.org/):

> 1. The contribution was created in whole or in part by you and you have the
>    right to submit it under the open source license indicated in the file; or
> 2. The contribution is based upon previous work that, to the best of your
>    knowledge, is covered under an appropriate open source license and you have
>    the right under that license to submit that work with modifications,
>    whether created in whole or in part by you, under the same license (unless
>    you are permitted to submit under a different license), as indicated in the
>    file; or
> 3. The contribution was provided directly to you by some other person who
>    certified (1), (2) or (3) and you have not modified it.
> 4. You understand and agree that this project and the contribution are public
>    and that a record of the contribution (including all personal information
>    you submit with it, including your sign-off) is maintained indefinitely and
>    may be redistributed consistent with this project and the requirements
>    stated above.

Your `Signed-off-by` line certifies the DCO above **and** accepts the
[CLA](CLA.md) for the signed contribution.
```

- [ ] **Step 2: Verify the anchor link resolves**

Run: `grep -n '^## Developer Certificate of Origin' CONTRIBUTING.md`

Expected: one match — confirms the `#developer-certificate-of-origin`
anchor referenced earlier in the file has a real heading to resolve to
(GitHub/most Markdown renderers slugify `## Developer Certificate of
Origin` to exactly that anchor).

- [ ] **Step 3: Commit**

```bash
git add CONTRIBUTING.md
git commit -m "Add CLA gate + DCO text to CONTRIBUTING.md

Replaces the now-incorrect dual Apache-2.0/MIT license section with a
CLA-gate section (matching aivyx-pa/CONTRIBUTING.md's own placement)
and a Developer Certificate of Origin section at the end -- contributor
sign-off (git commit -s) now certifies both, matching the mechanism
CLA.md's own 'How to accept' section describes."
```

---

### Task 5: `README.md` docs-correction

**Files:**
- Modify: `README.md:10`

**Interfaces:**
- Consumes: `LICENSE` (Task 2) — the new badge links to it.
- Produces: nothing later tasks depend on.

A repo-wide search already confirmed the license badge on line 10 is
the **only** "open source"/"MIT"/"Apache-2.0" self-claim about
`aivyx-coder`'s own code anywhere in `README.md`, `CLAUDE.md`, or
`ROADMAP.md` — no other files need this correction.

- [ ] **Step 1: Replace the license badge**

In `README.md`, change:

```markdown
[![License: Apache-2.0 OR MIT](https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue.svg)](LICENSE)
```

to:

```markdown
[![License: BUSL-1.1](https://img.shields.io/badge/license-BUSL--1.1-blue.svg)](LICENSE)
```

- [ ] **Step 2: Confirm no stale license text remains**

Run: `grep -niE "open.source|Apache-2\.0 OR MIT|MIT OR Apache" README.md CLAUDE.md ROADMAP.md`

Expected: no output.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "Update README license badge to BUSL-1.1"
```

---

### Task 6: Version bump + release prep

**Files:**
- Modify: `Cargo.toml` (root — `[workspace.package] version`)
- Modify: `crates/aivyx-acp/Cargo.toml`, `crates/aivyx/Cargo.toml`, `crates/aivyx-llm/Cargo.toml`, `crates/aivyx-mcp-server/Cargo.toml`, `crates/aivyx-core/Cargo.toml`, `crates/aivyx-tui/Cargo.toml`, `crates/aivyx-tools/Cargo.toml` (the 7 manifests with hardcoded `version = "0.1.0"` path-dependency pins)
- Modify: `Cargo.lock` (regenerated by `cargo check`)
- Modify: `docs/HISTORY.md` (append an entry)

**Interfaces:**
- Consumes: every previous task's changes — this is the final task, closing out the relicense.
- Produces: a local (unpushed) git tag `v0.2.0`.

- [ ] **Step 1: Bump the workspace version**

In `Cargo.toml`, in `[workspace.package]`, change:

```toml
version = "0.1.0"
```

to:

```toml
version = "0.2.0"
```

- [ ] **Step 2: Bump every hardcoded path-dependency version pin**

7 crate manifests declare sibling path dependencies with an explicit
`version = "0.1.0"` requirement alongside `path = "..."` (this is
*separate* from `version.workspace = true`, which each crate's own
`[package]` block already uses correctly and needs no change). Left
unbumped, these break the build — verified directly: bumping only the
workspace version above and running `cargo check -p aivyx` fails with
`failed to select a version for the requirement 'aivyx-acp = "^0.1.0"'`
because the actual crate is now `0.2.0`.

```bash
sed -i 's/version = "0.1.0"/version = "0.2.0"/' \
  crates/aivyx-acp/Cargo.toml \
  crates/aivyx/Cargo.toml \
  crates/aivyx-llm/Cargo.toml \
  crates/aivyx-mcp-server/Cargo.toml \
  crates/aivyx-core/Cargo.toml \
  crates/aivyx-tui/Cargo.toml \
  crates/aivyx-tools/Cargo.toml
```

- [ ] **Step 3: Verify the whole workspace builds**

Run: `cargo check --workspace`

Expected: succeeds with no errors. This also regenerates `Cargo.lock`
with the bumped internal crate versions.

- [ ] **Step 4: Run the full test suite**

Run: `cargo test --workspace`

Expected: all tests pass (same pass/fail state as `main` before this
plan started — this plan touches no application logic, only
manifests/docs, so no test's behavior should change).

- [ ] **Step 5: Re-run the license gate one more time**

Run: `cargo deny --all-features check licenses`

Expected: still `licenses ok` — confirms the version bump didn't
introduce a new dependency edge that changes the graph.

- [ ] **Step 6: Append a `docs/HISTORY.md` entry**

Read the tail of `docs/HISTORY.md` first to match its existing prose
style (it's a narrative log, one entry per initiative — see the most
recent entries for tone and structure). Append a new entry describing
this relicense: BUSL-1.1 replacing MIT OR Apache-2.0, the `cargo-deny`
audit finding (clean, unlike `aivyx-pa`'s own history), the new
`CLA.md`/`COMMERCIAL.md`/`TRADEMARK.md`/`CONTRIBUTING.md` CLA gate, and
the `v0.2.0` release this lands in. Write it in the same first-person
technical-narrative voice as the surrounding entries — do not invent
specifics beyond what this plan's tasks actually did.

- [ ] **Step 7: Commit the version bump and HISTORY.md entry**

```bash
git add Cargo.toml crates/*/Cargo.toml Cargo.lock docs/HISTORY.md
git commit -m "Bump version to 0.2.0 for the BUSL-1.1 release

Workspace version and every hardcoded sibling-crate version pin move
together (the latter break the build otherwise -- verified directly).
docs/HISTORY.md records this chapter."
```

- [ ] **Step 8: Create the release tag locally (do NOT push)**

```bash
git tag -a v0.2.0 -m "v0.2.0: relicense to BUSL-1.1

Aivyx Coder moves from MIT OR Apache-2.0 to the Business Source License
1.1 -- source-available, free for personal/non-commercial use, a paid
commercial license for business/production use, converting to MIT four
years after this release. See LICENSE, COMMERCIAL.md, and the
ecosystem-wide policy at https://github.com/Aivyx-Agent/aivyx-ecosystem/blob/main/LICENSING.md."
```

**This tag stays local.** Pushing it (`git push origin v0.2.0`) fires
`.github/workflows/release.yml` and creates a real public GitHub
release — that push needs explicit operator confirmation outside this
plan's automation, the same way any other push-to-remote does.

- [ ] **Step 9: Report final state**

Run: `git log --oneline -8` and `git tag --list`

Confirm: 6 new commits since this plan started (one per task), plus the
new local `v0.2.0` tag pointing at the last one.
