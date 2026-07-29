# Docker Model Runner Serving Support — Design

**Status:** Approved by user 2026-07-30.

## Context

aivyx-coder currently documents three local-LLM serving setups in
`README.md`'s "Serving" section: Ollama (zero-setup default), llama-server
(recommended for serious use), and Lemonade (a distro-packaged llama.cpp
wrapper) — plus a brief vLLM compat mention elsewhere in the repo
(`ROADMAP.md`/`docs/HISTORY.md`). Docker Model Runner (DMR), a feature of
Docker Desktop/Engine for running local models via an OpenAI-compatible
API, is not yet documented as a supported option.

This was originally raised as part of a larger idea — packaging
`aivyx-coder` itself as a Docker/container-based distribution, with DMR as
the bundled LLM backend, to simplify end-user setup. That larger question
was explicitly descoped after research surfaced a real, unresolved risk:
this project's core security mechanism (Landlock, a Linux kernel LSM
confining processes aivyx-coder itself spawns) is very likely blocked by
Docker's *default* seccomp profile (the `landlock_create_ruleset`/
`landlock_add_rule`/`landlock_restrict_self` syscalls are almost certainly
not on Docker's default allowlist), which would mean a plain
containerized `aivyx-coder` either refuses to run confined commands
(`sandbox.require_enforcement`'s fail-closed default) or silently runs
them unconfined. This could not be verified empirically in the research
session (no running Docker daemon was accessible), and is a real design
question in its own right — logged as a separate, future consideration,
not part of this spec. **This spec is scoped to only the smaller,
separable piece: documenting Docker Model Runner as a supported LLM
*serving backend* for aivyx-coder, which remains distributed exactly as
it is today (a native binary).**

## Research findings (external, not yet live-verified)

Everything below comes from Docker's own documentation and third-party
write-ups, gathered during this design's brainstorming phase — **none of
it has been confirmed against a real, running Docker Model Runner
instance**, unlike every other backend already documented in this
project (each of which was live-tested before being written down; see
`docs/HISTORY.md`'s Phase 10 and the vLLM compat-pass entry). This is
called out explicitly rather than silently assumed correct:

- **Base URL**: `http://localhost:12434/engines/v1` (host access), with
  chat completions at `POST /engines/v1/chat/completions`. An explicit
  engine name can optionally be included in the path:
  `/engines/llama.cpp/v1/chat/completions`.
- **Function/tool calling**: documented as supported, backed by
  llama.cpp, for compatible models.
- **Model naming**: `namespace/name[:tag]`, e.g. `ai/qwen2.5-coder`,
  `ai/smollm2:360M-Q4_K_M`. Docker's own model library, pulled via
  `docker model pull <name>` (OCI-artifact-based, similar in spirit to
  Ollama's own model pull mechanism but through Docker's registry
  tooling).
- **Context window**: llama.cpp-backed engines default to a 4096-token
  context unless explicitly configured — the same shape of trap this
  project already documents prominently for Ollama. `docker model
  configure --context-size N <model>` (or a `context_size:` key in a
  Docker Compose `models:` block) sets it explicitly. One third-party
  write-up (dated within the last several months) found a specific
  Docker CUDA runtime image that hard-coded `--ctx-size 4096` regardless
  of the `configure` setting — possibly a bug specific to that image/
  version, possibly already fixed; not something to assert as still-true
  without checking a current install.
- **Diagnostic/introspection endpoint**: unclear. Docker's docs mention
  an "Ollama-compatible `/api/show` endpoint" exists, but don't document
  its exact path relative to the `/engines/...` prefix, nor confirm it
  returns a `num_ctx`-bearing payload shape compatible with this
  project's existing `parse_ollama_show` parser. Not confirmed either way
  whether DMR exposes anything resembling llama-server's own `/props`
  (which the underlying llama.cpp process might, in principle, expose,
  the way Lemonade's underlying `llama-server` process does — but DMR's
  process-management layer may not surface it the same way).

## Decisions

### No code changes

`aivyx-llm`'s `OpenAiCompatBackend` treats `base_url` as an opaque prefix
and appends `/chat/completions` directly
(`crates/aivyx-llm/src/openai_compat.rs:136`) — this already works with
any OpenAI-compatible endpoint regardless of path depth, so pointing
`base_url` at `http://localhost:12434/engines/v1` (or the explicit-engine
variant) requires no change to the request path itself. This mirrors the
vLLM compat pass, which also needed zero new config surface.

`probe.rs`'s automatic context-window detection is **not** extended for
DMR in this spec. Its origin-derivation
(`base_url.trim_end_matches('/').trim_end_matches("/v1")`,
`crates/aivyx-llm/src/probe.rs:32`) only strips a trailing `/v1`, which
for DMR's `.../engines/v1` base URL leaves `.../engines` as the computed
"origin" — an assumption that isn't confirmed to line up with wherever
DMR's actual diagnostic endpoint (if any) lives. Given the diagnostic
endpoint's shape isn't confirmed at all, guessing an implementation now
would mean shipping unverified parsing logic — even though a wrong guess
fails safe (both existing parsers use `?`-chained `Option` lookups that
simply return `None`/fall through to `ServedContext::Unknown` on any
shape mismatch, never panicking or misreporting a wrong number), it's
still speculative code with no way to confirm it actually helps anyone
until someone can test it live. Consistent with this project's
established practice (see `docs/HISTORY.md`'s `diffy` and `cargo test`
path-filtering findings) of verifying a dependency's/service's actual
behavior before writing code against assumptions about it, this is left
as an explicitly named follow-up rather than guessed at now.

### Documentation: a new "Docker Model Runner" subsection

`README.md`'s "Serving" section gains a new subsection, positioned after
Lemonade's (matching that section's existing ordering: zero-setup
options first, more specialized ones after), covering:

- What Docker Model Runner is, in one sentence, and that it requires
  Docker Desktop or Docker Engine with the Model Runner feature enabled.
- The `base_url` to configure (`http://localhost:12434/engines/v1`) and
  the model-naming convention (`namespace/name[:tag]`).
- The context-window trap, written with the same directness as the
  Ollama section: default is 4096 unless explicitly configured via
  `docker model configure --context-size N <model>`, and — the honest
  caveat — this project's own startup probe does **not** currently detect
  this for DMR the way it does for Ollama, so a user should confirm their
  configured context size manually rather than relying on aivyx-coder to
  warn them.
- An explicit, visible note (not buried) that this section's technical
  details come from external research and have not yet been confirmed
  against a real running instance, with a pointer to what "confirming
  it" would mean (the same live-verification bar every other backend in
  this doc met) — matching how the ACP/Zed editor integration shipped
  with its own live-Zed-test explicitly named as a deferred follow-up
  rather than silently assumed complete.

### `ROADMAP.md` / `docs/HISTORY.md`

Given this ships with an explicitly-flagged unverified status (not the
usual "shipped and live-verified" bar every other Current-status entry
in `ROADMAP.md` meets), it should **not** be written as a plain "shipped"
paragraph alongside the others. Instead: a `ROADMAP.md` Current-status
paragraph noting it's documented but pending live verification (mirroring
the ACP chapter's own "shipped, one verification step still open"
framing), and a `docs/HISTORY.md` chapter with the same honesty —
recording what was researched, what remains unconfirmed, and naming the
live-verification follow-up explicitly, the same way `docs/HISTORY.md`
already tracks the still-open live-E2E items for `patch_file`/`move_file`/
`scoped_command`/`deny_paths`/`delegate_task` REPL isolation.

## Out of scope for this spec

- Containerizing aivyx-coder itself for distribution — a separate,
  larger initiative with a real, unresolved Landlock-vs-container-seccomp
  question, explicitly deferred (see Context above).
- Any `probe.rs` extension for DMR's context-window detection — deferred
  as a follow-up pending live access to a real DMR instance to confirm
  its actual diagnostic-endpoint shape (if one exists at all).
- Any new config field or `BackendSettings` change — `base_url` already
  covers this generically.

## Testing / verification

No automated tests are added — there is no new code. The verification
this spec calls for is entirely a live check, to be run once DMR is
accessible (on the bare-metal rig, or wherever the user next has it
available), matching the bar every other backend in this project has
already cleared:

- Confirm the documented `base_url` actually reaches a real DMR instance
  and a real chat completion round-trips successfully.
- Confirm (or correct) the exact context-window default behavior —
  including whether the reported CUDA-image hard-coding issue is still
  reproducible on whatever version is actually installed.
- Confirm whether tool/function calling works end-to-end through
  aivyx-coder's native edit format (not just "documented as supported"
  by Docker) — this project's own history (`docs/HISTORY.md`'s serving
  verdict) found that serving configuration, not the model, was the
  dominant reliability variable for tool-call correctness, so this is
  worth checking directly rather than assuming Docker's own claim
  transfers.
- If a real diagnostic/introspection endpoint is found and its shape
  confirmed, that's the trigger for a follow-up `probe.rs` extension —
  not part of this spec, but worth capturing as a concrete next step
  once the shape is known.

## Documentation

This spec's entire deliverable *is* documentation
(`README.md`/`ROADMAP.md`/`docs/HISTORY.md`) — see Decisions above for
the exact content shape.
