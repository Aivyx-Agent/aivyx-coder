# Docker Model Runner Serving Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Document Docker Model Runner (DMR) as a supported local-LLM
serving backend, honestly flagged as unverified against a real instance
— no code changes, since `base_url` already targets any OpenAI-compatible
endpoint generically.

**Architecture:** Pure documentation. `README.md`'s "Serving" section
gains a new subsection; `ROADMAP.md`'s Current status gains a paragraph
using a distinct "documented, pending live verification" framing (not
the "shipped" framing every fully-implemented-and-tested feature in that
file uses); `docs/HISTORY.md` gets a narrative chapter recording exactly
what's confirmed vs. still open.

**Tech Stack:** None — no code in this plan.

## Global Constraints

- No code changes anywhere in this plan — `crates/aivyx-llm` is not
  touched. If a future task ever extends `probe.rs` for DMR, that is
  explicitly a separate, later piece of work, not part of this plan.
- Every technical claim about Docker Model Runner's actual behavior
  (base URL, model naming, context-window default, tool-calling support)
  must be presented as externally-researched and **not yet live-verified**
  — this is a deliberate, visible departure from how every other backend
  in `README.md` is documented (each confirmed against a real running
  instance first), and the plan's own doc text must say so explicitly,
  not bury it in a footnote.
- `ROADMAP.md`'s Current-status entry for this item must use a distinct
  heading style from the "shipped" paragraphs around it (e.g. "documented,
  pending live verification"), since nothing was implemented in the
  usual sense — only documentation was written, and that documentation's
  own claims are unconfirmed.

---

### Task 1: Document Docker Model Runner in README, ROADMAP, and HISTORY

**Files:**
- Modify: `README.md`
- Modify: `ROADMAP.md`
- Modify: `docs/HISTORY.md`

**Interfaces:**
- Consumes: nothing (documentation only).

- [ ] **Step 1: Add the README "Docker Model Runner" subsection**

In `README.md`, find this existing text (the end of the Lemonade
subsection, right before the "## Editor integration (ACP)" heading):

```markdown
Once pointed correctly, everything else behaves exactly like a native
llama-server install: `ctx_size`/`--ctx-size` is an explicit first-class
control (no Ollama-style hidden default), and the Phase 10 acceptance
benchmark reproduced the native 9/9 / prompted 6/9 result exactly against
a Lemonade-managed `qwen3.5:9b`.

## Editor integration (ACP)
```

Insert a new subsection between them:

```markdown
Once pointed correctly, everything else behaves exactly like a native
llama-server install: `ctx_size`/`--ctx-size` is an explicit first-class
control (no Ollama-style hidden default), and the Phase 10 acceptance
benchmark reproduced the native 9/9 / prompted 6/9 result exactly against
a Lemonade-managed `qwen3.5:9b`.

**Docker Model Runner (Docker Desktop/Engine's built-in local model
runner).** ⚠️ **Not yet live-verified** — everything in this subsection
comes from Docker's own documentation and third-party write-ups
gathered during research, not from a real running instance (unlike
every other backend above, which was confirmed live before being
written down). Treat the specifics here as a starting point, not a
guarantee, until someone runs it.

Docker Model Runner (DMR) serves local models through an
OpenAI-compatible API, integrated into the normal Docker workflow —
pull a model with `docker model pull <name>` (model names look like
`ai/qwen2.5-coder` or `ai/smollm2:360M-Q4_K_M`), then point aivyx at:

```toml
[backend]
base_url = "http://localhost:12434/engines/v1"
model = "ai/qwen2.5-coder"
```

(An explicit engine can also be named in the path —
`http://localhost:12434/engines/llama.cpp/v1` — if the plain form
doesn't resolve on your install.)

**The same hidden-context-window trap as Ollama, reportedly**: DMR's
underlying llama.cpp engine defaults to a 4096-token context unless
explicitly configured. Set it with `docker model configure
--context-size N <model>` (or a `context_size:` key under `models:` in
a Docker Compose file) before pointing aivyx at it — and note that
aivyx's own startup probe (which catches this automatically for Ollama)
does **not** currently detect it for DMR, since DMR's diagnostic
endpoint shape isn't confirmed yet (see `probe.rs`). Until that's
extended, confirm your configured context size manually rather than
relying on a truncation warning. One third-party report (recent, but not
precisely dated) found a specific Docker CUDA runtime image that
hard-coded `--ctx-size 4096` regardless of the `configure` setting —
worth checking for on whatever version you actually install, not
assumed fixed or still-broken.

Tool/function calling is documented as supported (backed by llama.cpp),
but hasn't been checked end-to-end through aivyx's own native edit
format — this project's own experience is that serving configuration,
not the model, is usually the dominant variable for tool-call
reliability (see the Ollama-vs-llama-server serving verdict in
`ROADMAP.md`), so this is worth verifying directly rather than assuming
Docker's own claim transfers.

## Editor integration (ACP)
```

- [ ] **Step 2: Add the ROADMAP.md Current-status paragraph**

In `ROADMAP.md`, find this existing text (the end of the Landlock +
`aivyx-repomap` basename-glob enforcement entry, immediately before the
"## Backlog" heading):

```markdown
deliberate, justified duplicate this time, since the crate's real
architectural boundary (zero dependency on *other workspace crates*, not
zero external dependencies at all) stays intact.

See `docs/HISTORY.md` for the full phase-by-phase narrative behind
every item above.

## Backlog — capability opportunities, not yet scheduled
```

Insert a new paragraph between the feature entry and the closing
pointer line:

```markdown
deliberate, justified duplicate this time, since the crate's real
architectural boundary (zero dependency on *other workspace crates*, not
zero external dependencies at all) stays intact.

**Docker Model Runner serving support — documented, pending live
verification.** A new `README.md` "Serving" subsection covers Docker
Model Runner (DMR) as another local-LLM backend option — no code
changes needed, since `base_url` already targets any OpenAI-compatible
endpoint generically. Unlike every other backend documented in this
project, none of this subsection's technical claims (the exact `base_url`
path, the context-window default behavior, whether tool-calling works
end-to-end through aivyx's native edit format) have been confirmed
against a real running instance yet — see `docs/HISTORY.md` for the full
account of what's confirmed vs. still open.

See `docs/HISTORY.md` for the full phase-by-phase narrative behind
every item above.

## Backlog — capability opportunities, not yet scheduled
```

Also update the `_Last updated:_` line at the top of `ROADMAP.md` to
today's actual date, if it is not already today's date.

- [ ] **Step 3: Add a HISTORY.md chapter**

In `docs/HISTORY.md`, append a new chapter after the "### Landlock +
`aivyx-repomap` basename-glob enforcement — ✅ shipped" chapter (the last
chapter in the file):

```markdown
### Docker Model Runner serving support — documented, live verification pending

Originally raised as part of a larger idea — packaging aivyx-coder
itself as a Docker/container-based distribution, with Docker Model
Runner (DMR) as the bundled LLM backend, to simplify end-user setup.
That larger question was descoped immediately after research surfaced a
real, unresolved risk: this project's core security mechanism
(Landlock) is very likely blocked by Docker's *default* seccomp profile
— the `landlock_create_ruleset`/`landlock_add_rule`/`landlock_restrict_self`
syscalls are almost certainly not on Docker's default allowlist, based
on reasonably corroborated (but not empirically confirmed — no Docker
daemon was accessible in the research session) search findings. A
containerized aivyx-coder would likely either refuse to run confined
commands (`sandbox.require_enforcement`'s fail-closed default) or
silently run them unconfined, undermining the property `CLAUDE.md`
calls "load-bearing." This is logged as a separate, real, future design
question — not solved here, not part of this chapter.

**What actually shipped this chapter: a new `README.md` "Serving"
subsection documenting Docker Model Runner as a supported LLM backend**
— aivyx-coder itself stays a native binary, distributed exactly as
today. `aivyx-llm`'s `OpenAiCompatBackend` already treats `base_url` as
an opaque prefix and appends `/chat/completions` directly, so pointing
it at DMR needed zero code changes — the same "no new config surface
needed" shape as the earlier vLLM compat pass.

**Everything in the new subsection is honestly flagged as unverified**,
a deliberate departure from how every other backend in this project has
been documented — Ollama, llama-server, Lemonade, and vLLM were each
confirmed against a real running instance (live E2E tests, acceptance
benchmarks, or at minimum a manual compat check) before being written
into `README.md`. For DMR: no running Docker daemon was accessible
during this chapter's research, so the base URL
(`http://localhost:12434/engines/v1`), the model-naming convention
(`namespace/name[:tag]`), the reported hidden-context-window default
(4096, unless set via `docker model configure --context-size N`), and
whether tool/function calling actually works end-to-end through aivyx's
native edit format are all drawn from Docker's own docs and third-party
write-ups, not confirmed firsthand. One specific claim worth
double-checking on a real install: a third-party report found a Docker
CUDA runtime image that hard-coded `--ctx-size 4096` regardless of the
`configure` setting — possibly since fixed, possibly not.

**`probe.rs`'s automatic context-window detection was deliberately not
extended for DMR in this chapter.** Its origin-derivation logic
(`base_url.trim_end_matches('/').trim_end_matches("/v1")`) only strips a
trailing `/v1`; for DMR's `.../engines/v1` base URL this leaves
`.../engines` as the computed origin, an assumption not confirmed to
line up with wherever DMR's actual diagnostic endpoint (if one even
exists in an Ollama-`/api/show`-compatible shape) actually lives.
Shipping a guessed implementation would have been shipping unverified
parsing logic — low-risk, since both existing parsers fail safe to
`ServedContext::Unknown` on any shape mismatch rather than misreporting
a wrong number, but still speculative code with no way to confirm it
helps anyone until tested live. **General lesson, consistent with this
project's established practice** (see the `diffy` and `cargo test`
path-filtering findings elsewhere in this history): verify a
dependency's or service's actual behavior before writing code against
assumptions about it — documentation with an honest "unverified" label
is more useful than code that quietly might not work.

**Still open, the actual next step for this chapter**: a live check
against a real Docker Model Runner instance — confirming the base URL
and model-naming convention actually work, confirming or correcting the
context-window default behavior on whatever version is actually
installed, checking tool-calling end-to-end through aivyx's native edit
format, and — if a real diagnostic endpoint is found — a follow-up
`probe.rs` extension using the now-confirmed shape.
```

- [ ] **Step 4: Sanity check and commit**

Run `cargo build --workspace` to confirm nothing else in the repo was
accidentally touched or broken (this plan only edits Markdown files, but
this is the same sanity check every documentation-only task in this
project's history has run before committing).

```bash
git add README.md ROADMAP.md docs/HISTORY.md
git commit -m "Document Docker Model Runner as an unverified serving option"
```
