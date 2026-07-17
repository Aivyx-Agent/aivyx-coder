# Docs Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prepare aivyx-coder's documentation for its first public GitHub push: relocate the dev-journal content out of `ROADMAP.md` into `docs/HISTORY.md`, add missing repo-meta files (`LICENSE-MIT`, `LICENSE-APACHE`, `SECURITY.md`), and fix stale/broken cross-references and content in `README.md`/`CLAUDE.md`.

**Architecture:** Four independent-ish file changes: (1) split `ROADMAP.md` into a new lean summary plus a relocated `docs/HISTORY.md`, (2) add three new repo-meta files, (3) fix `README.md`/`CLAUDE.md` to point at the right file for each kind of content and correct known staleness, (4) verify the whole set is internally consistent. No code changes, no test suite applies — this is a docs-only change.

**Tech Stack:** Plain Markdown/text files only. No build tooling involved beyond `curl` (to fetch the canonical Apache-2.0 license text) and `git`/`grep`/`diff` for verification.

## Global Constraints

- Full authoritative spec: `docs/superpowers/specs/2026-07-18-docs-cleanup-design.md` — read it before starting; every decision below traces back to it.
- Copyright holder for both license files: `Julian`, year `2026`.
- License: `MIT OR Apache-2.0` (already declared in `Cargo.toml`'s `[workspace.package]` — do not touch `Cargo.toml`).
- `docs/HISTORY.md`'s content must be a **verbatim** copy of the pre-change `ROADMAP.md` except for exactly one added pointer sentence at the very top. Do not edit, trim, or "improve" any of the moved phase narrative.
- No code files (`crates/**`) are touched anywhere in this plan.
- No new repo-meta files beyond `LICENSE-MIT`, `LICENSE-APACHE`, `SECURITY.md` — `CONTRIBUTING.md` and issue/PR templates are explicitly out of scope (spec decision 6).
- No structural changes to `README.md` (no badges, screenshots, or install instructions) — accuracy fixes only (spec decision 7).

---

### Task 1: Relocate ROADMAP.md's history to docs/HISTORY.md, write a new lean ROADMAP.md

**Files:**
- Create: `docs/HISTORY.md` (verbatim copy of current `ROADMAP.md` + one pointer sentence)
- Modify (full rewrite): `ROADMAP.md`

**Interfaces:**
- Produces: `docs/HISTORY.md` at the repo root's `docs/` directory — Task 3 depends on this path existing for its cross-reference fixes.

- [ ] **Step 1: Copy the current ROADMAP.md verbatim to docs/HISTORY.md**

Run from the repo root:

```bash
cp ROADMAP.md docs/HISTORY.md
```

- [ ] **Step 2: Add the one pointer sentence at the very top of docs/HISTORY.md**

The current first three lines of `docs/HISTORY.md` (identical to the original `ROADMAP.md`) are:

```
# aivyx-coder Development Roadmap

_Last updated: 2026-07-08_
```

Use the Edit tool on `docs/HISTORY.md` with:

old_string:
```
# aivyx-coder Development Roadmap

_Last updated: 2026-07-08_
```

new_string:
```
*(This is the full phase-by-phase development history, relocated here
from `ROADMAP.md` on 2026-07-18. See `ROADMAP.md` at the repo root for
current status.)*

# aivyx-coder Development Roadmap

_Last updated: 2026-07-08_
```

This is the only edit made to `docs/HISTORY.md` — everything below it stays byte-identical to the pre-change `ROADMAP.md`.

- [ ] **Step 3: Verify the move was otherwise verbatim**

Step 2's edit prepended exactly 4 new lines to `docs/HISTORY.md` (3 lines
of new text plus 1 blank separator line) before the original content's own
first line (`# aivyx-coder Development Roadmap`, now at line 5):

```bash
diff <(git show HEAD:ROADMAP.md) <(tail -n +5 docs/HISTORY.md)
```

Expected: no output (empty diff) — `tail -n +5` skips exactly the 4 new
lines added in Step 2, so line 5 onward must match the original file's
line 1 onward exactly. If this produces any diff output, the copy was not
verbatim (or the line count is off — recount Step 2's new_string block if
so) — stop and fix before continuing.

- [ ] **Step 4: Overwrite ROADMAP.md with the new lean summary**

Use the Write tool to replace `ROADMAP.md`'s entire content with:

```markdown
# aivyx-coder Roadmap

_Last updated: 2026-07-18_

A terminal (TUI) coding agent for local LLMs only (Ollama, vLLM, or
llama.cpp) — see `README.md` for what it does and how to run it. This
file is the current-status summary; the full phase-by-phase history,
every design decision, and the evidence behind it lives in
`docs/HISTORY.md`. Every phase's original design spec and implementation
plan is tracked in `docs/superpowers/specs/` and `docs/superpowers/plans/`,
for the curious.

## Current status

**Shipped and live-verified** (Phases 1–8, 10 Parts A & B, 11a/11b/11c, 12,
and the full Phase 9 stretch-goal list): the full agent loop — streaming
chat, native + prompted SEARCH/REPLACE edit formats (A/B-measured, native
default), grep/glob search, `run_command`/`run_shell` behind real
Landlock+seccomp confinement, git tools (including `git_branch`/
`git_push`/`git_pr`) + automatic worktree checkpoint refs, `delete_file`
(`ActionKind::Delete`'s first constructor), tree-sitter repo map
injection, an agent-maintained wiki (`/wiki`), session persistence/resume,
context budget + compaction, plan mode (gate-enforced read-only),
autonomous mode (`--auto`), `/council` multi-model deliberation, AGENTS.md
project instructions, `web_fetch`/`web_search`, full MCP client support, a
startup probe of the *served* context window, goal-bounded turn pausing
instead of a hard iteration-cap failure, and enforced post-edit
verification with automatic fix-and-retry. 381 workspace tests; every
security-critical behavior also proven by live E2E against real serving.

**Serving verdict (Phase 10 Part A)**: the serving configuration — not the
model, not the edit format — was the dominant reliability variable.
Correctly-configured llama-server (explicit 16k window, thinking
disabled) took the same qwen3.5:9b from Ollama's best 7/9 to 9/9 at ~10×
the speed on the edit benchmark. The daily driver runs llama-server;
Ollama stays as the zero-setup default and serves the council's
swap-per-request members.

**Constrained-decoding verdict (Phase 10 Part B)**: SGLang's
xgrammar-forced tool calls matched llama-server's already-clean 9/9,
0-malformed-call baseline exactly — no material win, so no config surface
was added. Radix caching was confirmed to automatically reuse the entire
growing conversation prefix on every turn after the first, a genuine (if
not decision-gating) positive finding.

**vLLM compat pass**: the last README-claimed provider
(Ollama/vLLM/llama.cpp) is live-verified. vLLM served the same
Qwen3.5-9B-AWQ weights cleanly on the first attempt — no compat bugs,
unlike SGLang's dtype crash — with correct native AWQ quantization and
GDN/Mamba dtype auto-detection, and a working
`--default-chat-template-kwargs` launch flag SGLang lacked. No new config
surface needed — `base_url` already generically targets any
OpenAI-compatible endpoint.

**Capability audit — fully closed.** A broader audit against the
project's actual end-goal — a high-end vibe-coding agent with a path to
full autonomy — found the security/checkpoint foundation doesn't need a
redesign for autonomy, only extension, but surfaced two structural gaps
in the agent loop itself (closed via Phase 12) plus five smaller gaps
(closed via Phase 9: AGENTS.md, `web_fetch`/`web_search`, MCP client
support, branch/PR tooling, `delete_file`). No tracked items remain from
this audit.

**In flight / next**: nothing pre-scoped remains. All previously-tracked
threads (including the vLLM compat pass) are closed. See `docs/HISTORY.md`
for the full phase-by-phase narrative behind every item above.
```

- [ ] **Step 5: Commit**

```bash
git add ROADMAP.md docs/HISTORY.md
git commit -m "Docs: relocate phase history to docs/HISTORY.md, add lean ROADMAP.md"
```

---

### Task 2: Add LICENSE-MIT, LICENSE-APACHE, and SECURITY.md

**Files:**
- Create: `LICENSE-MIT`
- Create: `LICENSE-APACHE`
- Create: `SECURITY.md`

**Interfaces:**
- None — these are standalone new files with no dependency on Task 1 or Task 3.

- [ ] **Step 1: Write LICENSE-MIT**

Use the Write tool to create `LICENSE-MIT` with exactly this content:

```
MIT License

Copyright (c) 2026 Julian

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

- [ ] **Step 2: Fetch the canonical Apache-2.0 license text**

Don't hand-transcribe this from memory — it's long and a legal text, so fetch it from the canonical source to guarantee byte-for-byte accuracy:

```bash
curl -fsSL https://www.apache.org/licenses/LICENSE-2.0.txt -o /tmp/apache2-canonical.txt
wc -l /tmp/apache2-canonical.txt
```

Expected: succeeds, produces a file of roughly 200+ lines (the full Apache License 2.0 text ending in the "APPENDIX: How to apply the Apache License to your work" template with bracketed placeholders `[yyyy]`, `[name of copyright owner]`, etc.).

- [ ] **Step 3: Build LICENSE-APACHE from the fetched text with the copyright appendix filled in**

Read `/tmp/apache2-canonical.txt` with the Read tool. It ends with an appendix section containing a boilerplate notice template with placeholders like:

```
      Copyright [yyyy] [name of copyright owner]

      Licensed under the Apache License, Version 2.0 (the "License");
      ...
```

Use the Write tool to create `LICENSE-APACHE` containing the full fetched text verbatim, but with the appendix's placeholder notice filled in as:

```
   Copyright 2026 Julian

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
```

Every other line of the fetched text (the full license body above the appendix, and the appendix's own explanatory instructions) must be preserved exactly as fetched — only the placeholder notice at the very end is filled in with concrete values.

- [ ] **Step 4: Verify LICENSE-APACHE's license body matches the canonical source**

```bash
diff <(head -n 190 /tmp/apache2-canonical.txt) <(head -n 190 LICENSE-APACHE)
```

(Adjust `190` if the fetched file's appendix starts at a different line — the point is to diff everything above the filled-in copyright notice and confirm it's untouched. Inspect the fetched file's line count from Step 2 to pick the right cutoff.) Expected: no output, or only whitespace-equivalent differences. Any substantive diff means the license body was accidentally altered — fix before continuing.

- [ ] **Step 5: Write SECURITY.md**

Use the Write tool to create `SECURITY.md` with exactly this content:

```markdown
# Security Policy

aivyx-coder is a local-only tool — no server, no multi-tenant deployment.
The security boundary that matters is the sandbox/permission model:
`ActionKind` permission tiers, the `ConfirmationGate`, Landlock/seccomp
process confinement, and `deny_paths`. Vulnerabilities in scope are things
like sandbox escapes, permission-gate bypasses, or confinement gaps — not
"the LLM produced a bad answer" or similar model-quality issues.

Known, already-documented, accepted risk surface — not new findings — is
listed in `README.md`'s "Known limitations" section (indirect prompt
injection, unrestricted network access for approved commands, inherited
environment variables, TOCTOU windows on path resolution,
`AIVYX_DEBUG_LOG` plaintext logging, the `Tool::execute`
convention-not-type-system boundary, and git-specific caveats). Read that
section before reporting — if your finding is already listed there, it's
known and accepted, not a new report.

## Reporting a vulnerability

Email **jccorbett67@gmail.com** with details. This is a small,
solo-maintained project — there's no formal SLA, but reports will be read
and acknowledged.
```

- [ ] **Step 6: Commit**

```bash
git add LICENSE-MIT LICENSE-APACHE SECURITY.md
git commit -m "Docs: add LICENSE-MIT, LICENSE-APACHE, and SECURITY.md"
```

---

### Task 3: Fix README.md and CLAUDE.md — cross-references, staleness, Tools table

**Files:**
- Modify: `README.md`
- Modify: `CLAUDE.md`

**Interfaces:**
- Consumes: `docs/HISTORY.md` must already exist at the repo root's `docs/` directory (produced by Task 1) — every cross-reference fix below points at it.

This task depends on Task 1 being complete. Before starting, re-grep both files for `ROADMAP.md` to confirm line numbers match what's listed below — if the repo has changed since this plan was written, use the surrounding text (also given below) to relocate each edit rather than trusting line numbers alone.

- [ ] **Step 1: Fix README.md's phase-history pointer (near current line 12)**

Use the Edit tool on `README.md`:

old_string:
```
This is a from-scratch Rust project built reliability-first: the security
boundary was designed before the tools that need it, and hardened through
repeated full-codebase audits (see `ROADMAP.md` for the phase history).
```

new_string:
```
This is a from-scratch Rust project built reliability-first: the security
boundary was designed before the tools that need it, and hardened through
repeated full-codebase audits (see `docs/HISTORY.md` for the phase history).
```

- [ ] **Step 2: Fix README.md's Phase 2 reference (near current line 46)**

old_string:
```
get corrective feedback the model can retry from. See ROADMAP.md Phase 2
for the A/B measurements behind the default.
```

new_string:
```
get corrective feedback the model can retry from. See `docs/HISTORY.md`'s
Phase 2 for the A/B measurements behind the default.
```

- [ ] **Step 3: Fix README.md's Phase 11c reference (near current line 126)**

old_string:
```
Mutually exclusive with `--plan` and `--resume`. See ROADMAP.md's Phase 11c
entry for the full trust-profile rationale and design forks.
```

new_string:
```
Mutually exclusive with `--plan` and `--resume`. See `docs/HISTORY.md`'s
Phase 11c entry for the full trust-profile rationale and design forks.
```

- [ ] **Step 4: Fix README.md's Phase 11b reference (near current line 142)**

old_string:
```
first-run, full-skeleton regeneration. See ROADMAP.md's Phase 11b entry for
the full design rationale.
```

new_string:
```
first-run, full-skeleton regeneration. See `docs/HISTORY.md`'s Phase 11b
entry for the full design rationale.
```

- [ ] **Step 5: Fix README.md's Phase 10 / Lemonade reference (near current line 437)**

old_string:
```
Two things verified live (ROADMAP.md Phase 10) before pointing aivyx at
it:
```

new_string:
```
Two things verified live (`docs/HISTORY.md` Phase 10) before pointing aivyx
at it:
```

- [ ] **Step 6: Fix README.md's Phase 12 Part B reference in the config example (near current line 659)**

old_string:
```
# Enforced verification (ROADMAP.md Phase 12 Part B): after file edits,
```

new_string:
```
# Enforced verification (docs/HISTORY.md Phase 12 Part B): after file edits,
```

- [ ] **Step 7: Add the missing tools to README.md's Tools table (near current line 484)**

The table currently ends with the `git_commit` row. Use the Edit tool:

old_string:
```
| `git_read` | git status / diff / log (read-only, fixed argv shapes) | none (auto-allowed) |
| `git_commit` | stage + commit, with your identity/hooks/config | prompt, with a change-summary preview |

`run_command` and `run_shell` are only useful once you configure them (see
```

new_string:
```
| `git_read` | git status / diff / log (read-only, fixed argv shapes) | none (auto-allowed) |
| `git_commit` | stage + commit, with your identity/hooks/config | prompt, with a change-summary preview |
| `delete_file` | delete a file | prompt (then cacheable) |
| `git_branch` | create or switch git branches | prompt (then cacheable) |
| `git_push` | push the current branch to a remote | prompt (then cacheable) |
| `git_pr` | open a pull request via `gh` | prompt (then cacheable) |
| `web_fetch` | fetch a URL and convert to readable text | none (auto-allowed) |
| `web_search` | query a configured SearXNG instance | none (auto-allowed) |
| `go_to_definition` | resolve a symbol to its definition (via `rust-analyzer`) | none (auto-allowed) |
| `find_references` | find every reference to a symbol across the workspace | none (auto-allowed) |
| `delegate_task` | hand a bounded task to a fresh sub-agent | none (internal state only) |
| `list_mcp_resources` / `read_mcp_resource` | list/read resources from connected MCP servers | none (auto-allowed) |
| `list_mcp_prompts` / `get_mcp_prompt` | list/get prompts from connected MCP servers | none (auto-allowed) |
| `mcp__<server>__<tool>` | dynamically discovered tool from a connected MCP server | prompt (then cacheable) |

`run_command` and `run_shell` are only useful once you configure them (see
```

Every `ActionKind` used above was verified directly against source before
this plan was written: `delete_file` → `Delete`
(`crates/aivyx-tools/src/tools/delete_file.rs:76`); `git_branch`/
`git_push`/`git_pr` → `Execute`
(`crates/aivyx-tools/src/tools/git_branch.rs:119`,
`git_push.rs:89`, `git_pr.rs:125`); `web_fetch`/`web_search` → `Read`
(`web_fetch.rs:73`, `web_search.rs:84`); `go_to_definition`/
`find_references` → `Read` (`go_to_definition.rs:68`,
`find_references.rs:68`); `delegate_task` → `Internal`
(`crates/aivyx-core/src/delegate.rs:139`); the four MCP meta-tools → `Read`
(`crates/aivyx-tools/src/tools/mcp_meta.rs`, four occurrences); the
per-server MCP tool adapter → `McpTool`, target
`PermissionTarget::Other(...)` (`crates/aivyx-tools/src/tools/mcp_tool.rs:64-65`)
— never in the Read/Internal auto-allow tier (confirmed against
`crates/aivyx-sandbox/src/confirmation.rs`, which does not special-case
`ActionKind::McpTool` in the interactive Always-Allow caching path, only
in the separate autonomous-mode path), so it prompts like any other
confirm-gated action and is cacheable the same way afterward.

- [ ] **Step 8: Fix README.md's stale closing section (near current line 724)**

old_string:
```
See `ROADMAP.md` for what's planned next (repo map, git integration, richer
agentic UX) and the project's own audit history.
```

new_string:
```
See `ROADMAP.md` for current status and `docs/HISTORY.md` for the full
phase-by-phase history and audit trail.
```

- [ ] **Step 9: Fix CLAUDE.md's phase-history pointer (near current line 19)**

old_string:
```
The security boundary was designed before the tools that need it (see
`ROADMAP.md` for phase history) and is the load-bearing property of this
codebase — read the "Security model" section of `README.md` in full before
touching `aivyx-sandbox`, `aivyx-tools`, or the permission-gate logic in
`aivyx-core`.
```

new_string:
```
The security boundary was designed before the tools that need it (see
`docs/HISTORY.md` for phase history) and is the load-bearing property of
this codebase — read the "Security model" section of `README.md` in full
before touching `aivyx-sandbox`, `aivyx-tools`, or the permission-gate
logic in `aivyx-core`.
```

- [ ] **Step 10: Fix CLAUDE.md's "Where to look next" list (near current line 171)**

old_string:
```
- `ROADMAP.md` — phase-by-phase history and what's planned next, including
  the project's own audit history.
```

new_string:
```
- `ROADMAP.md` — current status, in brief.
- `docs/HISTORY.md` — full phase-by-phase history, every design decision
  and its evidence, including the project's own audit history.
```

- [ ] **Step 11: Verify no stale ROADMAP.md phase-reference remains**

```bash
grep -n "ROADMAP.md" README.md CLAUDE.md
```

Expected output: exactly two lines — the "See `ROADMAP.md` for current
status..." line from Step 8, and the "`ROADMAP.md` — current status, in
brief." line from Step 10. Every other match must have been converted to
`docs/HISTORY.md` in the steps above. If any other `ROADMAP.md` mention
remains, find and fix it before continuing.

- [ ] **Step 12: Commit**

```bash
git add README.md CLAUDE.md
git commit -m "Docs: fix stale ROADMAP.md cross-references and Tools table in README/CLAUDE"
```

---

### Task 4: Whole-repo consistency verification

**Files:**
- None modified — this task only verifies.

**Interfaces:**
- Consumes: the complete output of Tasks 1–3.

- [ ] **Step 1: Confirm every docs/HISTORY.md cross-reference resolves**

```bash
grep -n "docs/HISTORY.md" README.md CLAUDE.md
```

Expected: the set of lines fixed in Task 3 (Steps 1–6, 9, 10), each one a
plausible sentence referencing a real file. Manually confirm
`docs/HISTORY.md` exists and is non-empty:

```bash
test -s docs/HISTORY.md && echo "docs/HISTORY.md exists and is non-empty"
```

- [ ] **Step 2: Confirm docs/HISTORY.md hasn't been touched since Task 1**

Task 1 Step 3 already verified the move was verbatim (diffing the
pre-change `ROADMAP.md` against `docs/HISTORY.md`, while the old content
was still live in the working tree, before Task 1's own rewrite). This
step just confirms nothing in Tasks 2–3 accidentally touched
`docs/HISTORY.md` afterward:

```bash
git log --oneline -- docs/HISTORY.md
```

Expected: exactly one commit — Task 1's own commit
("Docs: relocate phase history to docs/HISTORY.md, add lean ROADMAP.md").
If more than one commit touches this file, inspect
`git diff <first-commit> <second-commit> -- docs/HISTORY.md` to see what
changed and confirm it was intentional (it shouldn't be — nothing in
Tasks 2–4 is supposed to modify this file).

- [ ] **Step 3: Confirm the Tools table row count**

```bash
grep -c "^| \`" README.md
```

Expected: `22` (10 original rows + 12 new rows from Task 3 Step 7). If the
count differs, re-check Task 3 Step 7 was applied correctly and no row was
duplicated or dropped.

- [ ] **Step 4: Confirm license files exist and are non-empty**

```bash
test -s LICENSE-MIT && echo "LICENSE-MIT ok"
test -s LICENSE-APACHE && echo "LICENSE-APACHE ok"
test -s SECURITY.md && echo "SECURITY.md ok"
grep -q "Copyright (c) 2026 Julian" LICENSE-MIT && echo "LICENSE-MIT copyright ok"
grep -q "Copyright 2026 Julian" LICENSE-APACHE && echo "LICENSE-APACHE copyright ok"
```

Expected: all five lines print.

- [ ] **Step 5: Full repo diff review**

```bash
git status --short
git diff --stat main
```

(If working in a worktree branched from `main`, `git diff --stat main`
shows the full set of changes across all three tasks' commits — confirm
the file list matches exactly: `ROADMAP.md`, `docs/HISTORY.md`,
`LICENSE-MIT`, `LICENSE-APACHE`, `SECURITY.md`, `README.md`, `CLAUDE.md`.
No `crates/**` files should appear.)

Expected: working tree clean (everything already committed in Tasks 1–3),
and the diff-stat file list matches exactly the seven files above — no
unexpected files, no code files.

- [ ] **Step 6: Report**

No further commit needed for this task (verification only). If any check
in Steps 1–5 failed, fix the underlying issue in the relevant earlier
task's files, commit the fix, and re-run the failed check before
considering this plan complete.
