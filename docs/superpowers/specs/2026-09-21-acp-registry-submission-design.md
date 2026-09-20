# ACP Registry Submission Design

## Context

`docs/superpowers/specs/2026-09-19-acp-registry-listing-design.md`
scoped the prerequisite work for submitting `aivyx-coder` to the ACP
(Agent Client Protocol) Registry (`github.com/agentclientprotocol/registry`)
and explicitly deferred the actual submission itself ("Decision 4:
release-platform expansion and the actual registry PR are out of scope
for the plan this spec produces"). That prerequisite work is confirmed
shipped: `crates/aivyx/src/setup_wizard.rs` (the `aivyx-coder setup`
subcommand), and a real `terminal` auth method wired in
`crates/aivyx-acp/src/session.rs`'s `terminal_auth_method()`, returned
from both `InitializeResponse` call sites via `.auth_methods(vec![...])`.
This spec covers the deferred piece: actually authoring and submitting
the registry PR.

## Grounding

Read/fetched directly, not assumed:

- `https://github.com/agentclientprotocol/registry/blob/main/CONTRIBUTING.md`
  and the registry's `agent.schema.json` — the current `agent.json`
  schema. Required: `id` (`^[a-z][a-z0-9-]*$`), `name`, `version`
  (`^[0-9]+\.[0-9]+\.[0-9]+$`), `description`, `distribution` (object,
  `minProperties: 1`), `license_url`. Optional: `repository`, `website`,
  `authors`, `license` (SPDX id/expression), `icon`, `preview`.
  `authMethods` does **not** appear in `agent.json` at all — it's
  protocol-level, returned by the running agent's own `initialize`
  response (confirmed already wired, see Context above); the registry's
  own CI "authMethods presence" check must probe the live agent binary,
  not the submitted JSON.
- `https://raw.githubusercontent.com/agentclientprotocol/registry/main/claude-acp/agent.json`
  — a real, live example confirming the schema's practical shape (no
  separate template file exists; each `<id>/` directory in the registry
  is itself the reference).
- `https://agentclientprotocol.com/rfds/auth-methods` — confirms the
  `terminal` auth type's JSON shape (`id`/`name`/`description`/`type`/
  `args`/`env`) and that `env_var` is effectively retired ("does not
  generalize to remote transports," absent from the current schema
  version).
- Registry `FORMAT.md` — icon convention: monochrome, `fill="currentColor"`/
  `fill="none"` only (hardcoded colors fail validation); real submissions
  (e.g. `claude-acp/icon.svg`, `viewBox="0 0 1200 1200"`) use a large
  internal viewBox scaled down for display, not literal 16×16
  coordinates. Distribution: `binary`/`npx`/`uvx`, at least one required;
  `binary` is keyed per-platform (`linux-x86_64`, `darwin-aarch64`,
  etc.), each entry needs `archive` (a URL — `/latest/`-containing URLs
  are explicitly rejected), `sha256` (recommended), `cmd`, `args`,
  optional `env`.
- `gh release view --repo Aivyx-Agent/aivyx-coder` — confirmed the real,
  only tagged release is `v0.1.0` (2026-09-10), with exactly one asset:
  `aivyx-coder-v0.1.0-x86_64-linux-musl.tar.gz` (+ its `.sha256`
  sidecar). The `aarch64-apple-darwin` build target exists in
  `.github/workflows/release.yml` today but was added *after* `v0.1.0`
  was cut — no release has produced a real `darwin-aarch64` asset yet.
  Confirmed with the project owner: submit Linux-only now (valid per the
  registry's own partial-platform-coverage rule) rather than waiting for
  a fresh tag.
- Root `LICENSE` file (added 2026-09-11 per `ROADMAP.md`) — confirmed
  its real content: a dual-license pointer ("Apache License, Version 2.0
  ... or ... MIT license ... at your option"), pointing readers at
  sibling `LICENSE-APACHE`/`LICENSE-MIT` files. This is the single
  canonical URL target for `license_url` (one field, one URL — the
  dual-license nuance itself is expressed via the separate `license`
  field's SPDX expression, `"MIT OR Apache-2.0"`).
- `.github/workflows/release.yml:58` — confirms `LICENSE-MIT`/
  `LICENSE-APACHE` are staged into each release archive already,
  consistent with the dual-license framing above.

## Decisions

**1. Two-stage process: author + validate locally, then a separate,
explicitly-confirmed external PR step.** Stage 1 produces
`docs/registry-submission/aivyx-coder/agent.json` and
`docs/registry-submission/aivyx-coder/icon.svg` inside *this* repo (a
review-friendly staging location, git-tracked so the exact submitted
content has a real history here too) and validates both offline before
either file ever touches the external fork. Stage 2 — forking
`agentclientprotocol/registry`, adding the two files at the fork's
`aivyx-coder/` root, committing, pushing, and opening the PR — happens
only after the project owner has reviewed Stage 1's final content and
explicitly confirmed. This mirrors how every other real external/
irreversible action in this project's process has been handled: confirm
before, not after.

**2. `agent.json` content, fully grounded in real, current repo state**:

```json
{
  "id": "aivyx-coder",
  "name": "aivyx-coder",
  "version": "0.1.0",
  "description": "A terminal coding agent for local LLMs only (Ollama, vLLM, llama.cpp) -- never calls a cloud API.",
  "repository": "https://github.com/Aivyx-Agent/aivyx-coder",
  "license": "MIT OR Apache-2.0",
  "license_url": "https://github.com/Aivyx-Agent/aivyx-coder/blob/main/LICENSE",
  "icon": "icon.svg",
  "distribution": {
    "binary": {
      "linux-x86_64": {
        "archive": "https://github.com/Aivyx-Agent/aivyx-coder/releases/download/v0.1.0/aivyx-coder-v0.1.0-x86_64-linux-musl.tar.gz",
        "sha256": "<the real published digest, copied verbatim from the release asset's own .sha256 sidecar at plan/execution time, not retyped by hand>",
        "cmd": "aivyx-coder",
        "args": ["--acp"]
      }
    }
  }
}
```

Exact final field values (especially the real `sha256`, and confirming
`version`/`id` pass the registry's regex and slug-uniqueness checks)
are pinned at plan-writing/implementation time by reading the live
release asset directly — this spec fixes the *shape* and *sourcing*,
not a hand-copied digest that could go stale or be mistyped. `args:
["--acp"]` is required — the registry only cares about an agent's ACP
server mode, and `aivyx-coder`'s default (no-subcommand) behavior is
the interactive TUI, not the ACP frontend.

**3. Icon: a new, purpose-built monochrome mark — a terminal-prompt
chevron (`>`) with a cursor/underscore**, not adapted from any existing
`aivyx-brand` asset (a deliberate choice, confirmed with the project
owner: the brand's existing marks weren't built for this tiny,
monochrome, `currentColor`-only constraint, and a fresh purpose-built
glyph avoids compromising either the brand asset's own design intent or
the registry icon's legibility at effective 16px size). Single
`<path fill="currentColor">`, large internal viewBox (matching the
`claude-acp` precedent, e.g. `viewBox="0 0 24 24"` or similar, scaled
for display, not a literal 16×16 coordinate space), no hardcoded colors
anywhere in the file (`fill="currentColor"`/`fill="none"` only — a hard
registry CI requirement, not a style preference).

**4. Offline validation before the external PR**: fetch the registry's
real, current `agent.schema.json` and validate the drafted `agent.json`
against it directly (a real JSON Schema conformance check, not eyeballing),
plus a manual grep of `icon.svg` for any hardcoded color value (`#`/named
CSS colors) to confirm only `currentColor`/`none` appear. Both checks run
against the Stage 1 staged files before Stage 2 ever begins.

## What this spec does not decide

- `darwin-aarch64` distribution — deliberately deferred; a later,
  separate PR once a real tagged release actually produces that binary
  asset (confirmed with the project owner as the sequencing choice).
- Registry maintainer review/merge timeline or any back-and-forth their
  own review might require — outside this project's control, not
  designed for here.
- `npx`/`uvx` distribution methods — `aivyx-coder` is a compiled Rust
  binary with no npm/PyPI package; `binary` distribution is the only
  applicable method, so `npx`/`uvx` are never considered.
- Whether to eventually add a `website` field pointing at
  `aivyx-website` — optional field, not decided here; can be added in a
  later, separate PR if desired.
