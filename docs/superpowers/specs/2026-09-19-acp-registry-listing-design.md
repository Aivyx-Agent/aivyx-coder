# ACP Registry Listing Design

## Context

`aivyx-coder` already has a real ACP (Agent Client Protocol) frontend
(`crates/aivyx-acp`, shipped 2026-07-20 — see
`docs/superpowers/specs/2026-07-20-acp-editor-integration-design.md`)
reaching Zed's Agent panel and (via the third-party `formulahendry.acp-client`
extension) VS Code. Today it's reachable only by a user hand-writing a
`settings.json` `agent_servers` entry pointing at a locally-built binary.

The original ask was "a Zed Extension for aivyx-coder." Direct research
found that's the wrong vehicle: Zed deprecated extension-provided agents
as of v1.5.0 in favor of the **ACP Registry**
(`github.com/agentclientprotocol/registry`) — a neutral, protocol-level
registry (not Zed-specific) that Zed, JetBrains, and any other
ACP-speaking client pull from, updated hourly. Submission is a PR adding
`<agent-id>/agent.json` + a 16×16 monochrome `icon.svg`, not a WASM build.

## Grounding

Read/fetched directly, not assumed:

- `https://zed.dev/docs/extensions/agent-servers` — confirms "ACP
  extensions have been deprecated in favor of the ACP Registry" as of
  Zed v1.5.0.
- `https://github.com/agentclientprotocol/registry/blob/main/CONTRIBUTING.md`
  — the full `agent.json` schema, CI validation rules, and submission
  process (fork → add `<id>/agent.json` + `<id>/icon.svg` → PR → CI
  validates schema, slug uniqueness, icon format, distribution URL
  accessibility, and `authMethods` presence).
- `https://agentclientprotocol.com/rfds/auth-methods` — the current
  `authMethods` schema. Exactly two live types: `agent` (agent handles
  OAuth itself, `auth/login`) and `terminal` (client launches the agent
  binary with the method's own `args`/`env` in an embedded terminal,
  waits for exit 0, reconnects). A third type, `env_var`, is explicitly
  **deprecated** — the RFD states it "does not generalize to remote
  transports" and instructs migration to `agent` or `terminal`. There is
  no documented "no authentication needed" type.
- `gh release view v0.1.0 --repo Aivyx-Agent/aivyx-coder` — confirms the
  existing release pipeline (`.github/workflows/release.yml`) currently
  builds exactly one target, `x86_64-linux-musl`.
- `crates/aivyx-acp/src/session.rs:190`,
  `crates/aivyx-acp/src/prompter.rs:427` — both construct
  `InitializeResponse::new(req.protocol_version)` with no
  `.auth_methods(...)` call; grepped the crate for
  `authMethods`/`auth_methods` — zero matches. The registry's CI-enforced
  requirement ("agent must return `authMethods`... at least one method
  must have `type: agent` or `type: terminal`") is not satisfied today.
- `crates/aivyx-config/src/lib.rs:1001-1034` — `Settings::load()`'s
  documented first-run behavior is silently writing default values to a
  fresh `config.toml`, not an interactive prompt flow. There is no
  existing setup wizard to repurpose as a genuine `terminal` auth entry
  point — building one is real, new scope, not a relabeling exercise.
- `crates/aivyx-llm/src/probe.rs` — `probe_served_context(base_url,
  model)` already exists: queries llama-server's `/props` or Ollama's
  `/api/show` for the real served context window, `PROBE_TIMEOUT` 3s,
  advisory-only (`ServedContext::{Known, OllamaDefaultUnknown, Unknown}`).
  Directly reusable by a setup wizard's verification step — not
  duplicated.
- `crates/aivyx-config/src/lib.rs:587-696` — `BackendSettings`/
  `BackendKind` (`Generic` default, `LlamaServer`, `LlamaServerBroker`,
  `MistralRs`). No existing "list available models" call anywhere in
  `aivyx-llm` — new, small code needed for that specific step.

## Decisions

**1. Two separable initiatives, not one plan.** The wizard + ACP
`terminal`-auth wiring is self-contained, independently valuable
(closes a real onboarding gap regardless of the registry), and fully
testable on its own. Release-platform expansion + the actual registry PR
depend on it being done first, but are their own, later scope — a
different kind of work (release engineering, an external PR against a
repo this project doesn't own) with a different risk profile. This spec
covers both; only the first becomes an implementation plan now.

**2. The setup wizard is a new subcommand**, `aivyx-coder setup` (final
name confirmed at plan time against the existing `clap` subcommand
list — `run` with no subcommand today is the default TUI launch, so
`setup` must not collide). Scope, in order:
   - Choose a backend: **Ollama** (default choice — zero-setup, the
     common case) or **"point at a running OpenAI-compatible server"**
     (llama-server/vLLM/other — manual `base_url` entry). `LlamaServerBroker`/
     `MistralRs` stay config.toml-only, not wizard paths — both need
     context (a running `aivyx-broker`, a compiled-in feature flag
     respectively) a first-run wizard shouldn't assume.
   - `base_url`, defaulted per choice (`http://localhost:11434/v1` /
     `http://localhost:8080/v1`), editable.
   - Model: for Ollama, list real locally-pulled models via `GET
     /api/tags` (new code) and let the user pick; for the generic path,
     try `GET /v1/models` (fairly standard on OpenAI-compatible servers)
     with graceful fallback to manual entry if unimplemented.
   - Verify via the existing `probe_served_context` — surfaces a real
     served-context-window mismatch at setup time instead of as silent
     mid-response truncation later, turning a known footgun
     (`aivyx-llm/src/probe.rs`'s own doc comment: "Ollama serves a
     4096-token default... regardless of configuration") into immediate,
     actionable feedback.
   - Write `config.toml` through the existing `Settings`/`BackendSettings`
     serialization — no new file format.
   - Must behave identically whether a human runs it directly in their
     own terminal, or a client (Zed/JetBrains) launches it embedded in
     their own terminal panel with the `terminal` auth method's `args`/
     `env` — both are real terminal emulators from the process's point of
     view, so no TTY-specific special-casing is expected to be needed,
     but this is worth confirming empirically at plan/test time rather
     than assumed.

**3. `InitializeResponse` gains a genuine `terminal` auth method**
pointing at `aivyx-coder setup` (with whatever `args`/`env` the ACP
`terminal` schema requires — e.g. an env var the wizard can check to
adjust its exit behavior/messaging when launched this way vs. run
directly by a human). This is the honest fit the earlier investigation
identified: not a relabeled no-op, but aivyx-coder's real (new) onboarding
flow, genuinely a "the agent isn't usable until this interactive step
completes" gate — which is exactly what `terminal` auth means
operationally, even though the RFD's own framing describes it as "login."

**4. Release-platform expansion and the actual registry PR are
out of scope for the plan this spec produces.** Recorded here as the
known next step, not silently dropped: today's pipeline builds
`x86_64-linux-musl` only; the registry allows partial platform coverage
(a Linux-only submission is valid, it just limits which editor-host OSes
get one-click install), so submission isn't blocked on full platform
coverage, but real reach means adding at least `darwin-aarch64` (the
dominant Zed/JetBrains dev machine) before or shortly after the initial
PR. The `agent.json`/`icon.svg` PR itself, and confirming CI's binary-URL
and `authMethods` checks pass for real (not just read about), is real,
separate follow-on work once this plan's `terminal` auth method exists to
point at.

## What this spec does not decide

- The exact `terminal` auth method's `id`/`name`/`description` copy, and
  the precise `args`/`env` shape — plan-time work once the exact
  `agent-client-protocol` crate version's Rust API for
  `.auth_methods(...)` is read directly (not assumed from the RFD's raw
  JSON alone).
- Whether the wizard should also be re-runnable to *change* an existing
  config (not just first-run) — a real UX question, not resolved here.
- `darwin-aarch64`/further platform build tooling specifics (cross-compile
  toolchain, CI runner choice) — separate, later scope per Decision 4.
- The actual `agent.json` content and icon design — separate, later scope
  per Decision 4.
