# Aivyx-Vision Image/3D Adoption (aivyx-coder) Design

## Context

`aivyx-coder` already adopted Aivyx-Vision's Milestone 1
(`generate_svg`, shipped 2026-09-18 — see
`docs/superpowers/plans/2026-09-18-vision-svg-adoption.md`): a tool in the
existing `aivyx-tools` crate, reusing the agent's own already-configured
`Arc<dyn LlmBackend>` (via a small adapter, `CoderTextCompleter`), gated
`ActionKind::Network` — the same auto-allow tier as `web_fetch`/
`web_search`, since `generate_svg` can only ever reach the one
operator-configured backend endpoint the turn loop already contacts every
turn regardless of any flag.

Aivyx-Vision's own repo just shipped Milestone 2 Pass A (2026-09-18,
commit `caed4c0`): `aivyx-vision-core` (the `GenerationProvider` trait +
types) and `aivyx-vision-mold` (an HTTP-client backend against an
operator-run `mold serve` instance, coordinated with local LLM inference
via `aivyx-broker`'s GPU lock). `generate_3d` on `MoldProvider` always
returns `VisionError::Unsupported` — mold's async 3D job API ("Pass B")
isn't built yet.

This design covers `aivyx-coder`'s adoption of that Milestone 2 Pass A
work: `generate_image`/`generate_3d` tools in the *existing* `aivyx-tools`
crate. `aivyx-pa`'s equivalent adoption (a separate product, a
structurally different tool-process architecture) already shipped — see
`docs/superpowers/specs/2026-09-18-vision-image-3d-adoption-design.md` in
that repo. **This design deliberately does not copy every decision from
that one** — `aivyx-coder`'s security model is architecturally different
in a way that changes several of them, detailed below.

## Grounding

Read directly from the current codebase (not assumed):

- `crates/aivyx-tools/src/tools/generate_svg.rs` — the template for a
  `Tool` impl backed by an Aivyx-Vision crate: `permission_request`/
  `execute` shape, `needs_checkpoint()` override pattern, test structure.
- `crates/aivyx-tools/src/tools/write_file.rs` — `permission_request`
  resolves the target via `crate::path_resolve::resolve(cwd, path)` and
  returns `ActionKind::Write` + `PermissionTarget::Path`; its own
  diff-preview read of the pre-existing file content is *not* separately
  gated — only the one declared `PermissionRequest` per call is checked
  by the gate.
- `crates/aivyx-tools/src/tools/read_file.rs` — accepts any absolute or
  cwd-relative path via the same `resolve()` helper, gated
  `ActionKind::Read` (auto-allowed).
- `crates/aivyx/src/agent_builder.rs` — `cwd` is resolved exactly once,
  at `build_agent()` startup (`std::env::current_dir()?.canonicalize()?`,
  line 183), and reused everywhere a tool needs it — confirming cwd is
  fixed for a whole session/process, not something that varies per call
  in practice. The existing `web_fetch`/`web_search` conditional
  registration (`if settings.web.enabled { ... }`, ~line 400) is the
  direct template for gating `generate_image`/`generate_3d` registration
  on new config, and its own comment explains exactly why `generate_svg`
  is deliberately *not* gated the same way (see Grounding's first bullet)
  — a distinction this design's own tools must reason about fresh, not
  copy blindly.
- `crates/aivyx-config/src/lib.rs`'s `WebSettings` (line 300) — the
  template `VisionSettings` (below) copies exactly: an `enabled: bool`
  field plus a `Default` impl providing sane values for everything else.
- `README.md`'s "Tools" table (line 1157) and `generate_svg`'s own prose
  section (line 532) — confirmed directly, not assumed: `ActionKind::Network`
  is *also* an auto-allow tier here (same as `Read`/`Internal`), not a
  confirmation tier — this is the concrete fact that makes `ActionKind::Write`
  (which genuinely does prompt, per `write_file`'s own README row,
  "prompt (then cacheable)") the materially more conservative choice for
  a tool with a real filesystem side effect, not just a stylistic
  preference.
- Root `Cargo.toml:34` — `aivyx-vision-svg` is a pinned git dependency at
  `rev = "a80be4709b41382ef62c20545bb706e18a1d5ee3"` (tip of
  `aivyx-vision`'s `main` immediately before Milestone 2 Pass A merged) —
  same stale-pin situation `aivyx-pa` had.
- `crates/aivyx-types/src/lib.rs`'s `ToolOutput` enum (line 86) —
  `Ok(String)`/`Error(String)`/`Denied(String)` only, no structured JSON
  variant; `write_file`'s own success message
  (`format!("wrote {} bytes to {}", ...)`) is the template for
  `generate_image`'s own success string.

## Why this design differs from `aivyx-pa`'s

`aivyx-pa`'s `vision.generate_image` tool runs in a **separate OS
process** with no visibility into the daemon's own fs sandbox — that's
why its `reference_image` field was restricted to a bare filename inside
the tool's own output directory (any arbitrary path would have been an
unguarded exfiltration route with zero human review).

`aivyx-coder` has no such gap: every tool call — `generate_image`
included — runs **in-process**, through the exact same
`ConfirmationGate`/`deny_paths`/human-confirmation pipeline every other
tool (`read_file`, `write_file`, `grep`, ...) already goes through. There
is nothing structurally special about a path `generate_image` might touch
that `read_file`/`write_file` don't already handle identically. Building
a bespoke restriction here would be solving a problem this product
doesn't have, at the cost of an inconsistent, harder-to-explain rule
("every tool accepts any path except this one"). So `reference_image`
just uses the existing `resolve()` helper, no extra checks — decision 3
below is close to trivial once this architectural difference is named
explicitly, which is the whole point of writing it down here rather than
silently porting `aivyx-pa`'s answer.

## Decisions

**1. New tools live in a new file, `crates/aivyx-tools/src/tools/generation_tools.rs`,
in the existing `aivyx-tools` crate — not a new crate.** Both depend on
`Arc<dyn aivyx_vision_core::GenerationProvider>` (the trait object),
never a concrete `aivyx-vision-mold` type — only `agent_builder.rs`
constructs the concrete `MoldProvider`. Mirrors `GenerateSvgTool`'s own
`Arc<dyn TextCompleter>` dependency shape.

**2. `generate_image` is `ActionKind::Write`, not `Network`.** Unlike
`generate_svg` (returns text only, no filesystem effect),
`generate_image` writes a real file into the project — the thing that
actually needs operator review is that write, not the network call that
produces its content. Concretely and consequentially: `Network` is an
**auto-allow tier here** (confirmed in `README.md`, same as
`web_fetch`/`web_search`/`generate_svg` — zero confirmation, ever);
`Write` genuinely prompts (then is cacheable), matching `write_file`.
Choosing `Write` is therefore not a style preference — it's the
difference between "the operator is asked once" and "the operator is
never asked at all." `needs_checkpoint()` is **not** overridden (unlike
`generate_svg`'s explicit `false` override) — it defaults to
`mutates_outside_session()`'s own default (`true`), so a generated file
gets a git checkpoint like any other real write, matching `write_file`'s
own (absence of an) override.

**3. `reference_image` is a plain path through the existing
`resolve(cwd, path)` helper — no extra restriction.** See "Why this
design differs from `aivyx-pa`'s" above for the full reasoning. Concretely:
`GenerateImageArgs.reference_image: Option<String>`, resolved exactly
like `read_file`'s/`write_file`'s own `path` argument, read directly (not
separately permission-gated — matching `write_file`'s own ungated
diff-preview read of pre-existing file content). The tool's one declared
`PermissionRequest` covers the write; reading a reference image to inform
that write is no more separately gated than `write_file` reading the file
it's about to overwrite.

**4. The permission target is the `assets/generated/` directory itself,
not a specific future filename.** `write_file`'s Always-Allow cache keys
on the exact target path deliberately (approving `write a.rs` must never
bless `write b.rs`) — but `generate_image`'s actual output filename is a
fresh UUID minted by `MoldProvider` after the call, never something the
operator picks or could meaningfully review ahead of time. Targeting the
directory means one approval covers "this agent may generate images into
`assets/generated/`" for the rest of the session, which matches what's
actually operator-meaningful here (confirmed with the project owner) —
targeting a not-yet-known specific filename would be a fiction, and
re-prompting on every call for a filename the operator can't meaningfully
evaluate would be pure friction with no real review value.

**5. `generate_3d` ships now, not deferred to Pass B** — same reasoning
and same decision as `aivyx-pa`'s adoption: every call fails today with
`VisionError::Unsupported`'s message, surfaced as `ToolOutput::Error`, but
the tool is discoverable now and starts working automatically once a real
3D backend replaces `MoldProvider`'s stub, with zero changes to this
crate.

**6. Output lands at `<cwd>/assets/generated/<uuid>.<ext>` — fixed, not
configurable.** Matches the ecosystem spec's own explicit recommendation
for this product ("default output under `assets/generated/`,
workspace-relative"). `cwd` is resolved once at `build_agent()` startup
(already true today for every other cwd-dependent tool), so
`GenerateImageTool`'s backing `MoldProvider` is constructed once there
too, with `output_dir: cwd.join("assets/generated")` — no per-call path
resolution, no config override field. Simpler than `aivyx-pa`'s
`$HOME`-based default-with-override, because this product's whole config
philosophy is project-relative, not home-dir sprawl.

**7. Config: a new `VisionSettings` on `aivyx-config::Settings`,
mirroring `WebSettings` exactly.**

```rust
pub struct VisionSettings {
    pub enabled: bool,
    pub broker_url: String,
    pub mold_url: String,
    pub api_key: Option<String>,
}

impl Default for VisionSettings {
    fn default() -> Self {
        Self {
            enabled: false, // opt-in -- needs external infra (aivyx-broker + mold serve)
            broker_url: "http://127.0.0.1:8899".to_string(),
            mold_url: "http://127.0.0.1:7680".to_string(),
            api_key: None,
        }
    }
}
```

```toml
# ~/.config/aivyx-coder/config.toml
[vision]
enabled = true
broker_url = "http://127.0.0.1:8899"
mold_url = "http://127.0.0.1:7680"
# api_key = "..."
```

`agent_builder.rs` registers both tools only when `settings.vision.enabled`
— the exact same conditional-registration shape already used for
`web_fetch`/`web_search`. `enabled = false` (the default) means zero
behavior change for every existing install. Unlike `generate_svg` (always
registered, deliberately not gated — see Grounding), `generate_image`/
`generate_3d` genuinely need this gate: they depend on external
infrastructure (`aivyx-broker`, `mold serve`) that isn't installed by
default, the same reason `mcp_server` and other opt-in features are
config-gated in this codebase. If `enabled` is `true` but
`MoldProvider::new(...)` fails to construct (the only realistic cause: the
underlying `reqwest::Client::builder().build()` call erroring, a rare
non-deterministic condition), the agent degrades gracefully — logs to
stderr and continues starting with neither tool registered, rather than
failing the whole process over an optional capability. Matches
`aivyx-pa`'s adoption's identical choice for the identical failure mode.

**8. Output message matches `write_file`'s own convention**: on success,
`ToolOutput::Ok(format!("generated image saved to {} (seed {})", path,
seed_display))` (seed rendered as its value or `"none"` if `None`); on
failure, `ToolOutput::Error(format!("generate_image: {e}"))`, matching
`generate_svg`'s own error-formatting shape.

**9. Dependency pinning: bump the existing `aivyx-vision-svg` pin and add
`aivyx-vision-core`/`aivyx-vision-mold`, all three at `caed4c0875a19ef5c7f4059ddda8cd6364f9fda4`**
— same rationale as `aivyx-pa`'s adoption: keep all three Aivyx-Vision
crates in lockstep rather than letting one drift.

## Testing

- `GenerateImageTool`/`GenerateThreeDTool` unit tests use
  `aivyx_vision_core::FakeGenerationProvider` — requires enabling that
  crate's `testing` Cargo feature under `[dev-dependencies]` only in
  `crates/aivyx-tools/Cargo.toml` (a real gap `aivyx-pa`'s own
  implementer found and fixed during that adoption; specified directly in
  this design so the plan doesn't have to rediscover it). Mirrors
  `generate_svg.rs`'s own `FakeCompleter` test pattern: permission-tier
  assertion (`action == ActionKind::Write`), success/error/missing-argument
  execute() cases, plus a `needs_checkpoint()`/`mutates_outside_session()`
  assertion pair analogous to `generate_svg`'s own (but asserting `true`/
  `true`, the *un*-overridden defaults, not `generate_svg`'s `true`/`false`
  split).
- No test requires a real `mold serve`/`aivyx-broker` process — matches
  `aivyx-vision-mold`'s own CI-never-needs-a-GPU constraint.

## Documentation

- `README.md`'s "Tools" table: two new rows,
  `generate_image` / `generate_3d`, both "prompt (then cacheable)" for
  Confirmation.
- `README.md`: a new prose section (immediately after `generate_svg`'s,
  ~line 548) documenting `[vision]` config, the `assets/generated/`
  output location, the directory-level Always-Allow caching behavior
  (decision 4), and that `generate_3d` isn't implemented yet.

## What this design does not decide (explicitly out of scope)

- Pass B itself (a real `generate_3d` implementation).
- `aivyx-vision-comfyui` (deferred entirely, per the ecosystem spec's own
  2026-09-18 amendment).
- Any budget/rate-limiting integration — `aivyx-coder` has no equivalent
  of `aivyx-pa`'s Chapter K cost-governance crate at all; not applicable
  here.
- Retention/cleanup of accumulated generated files under
  `assets/generated/` — operator-managed, matching the ecosystem spec's
  own v1 default.
- The GPU-lock async-cancellation lease-leak gap noted in
  `aivyx-vision-mold`'s own final review — a pre-existing gap in the
  engine crate this adoption depends on, not something either product's
  adoption plan can fix.
