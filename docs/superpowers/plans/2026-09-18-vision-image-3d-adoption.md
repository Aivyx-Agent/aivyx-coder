# Vision Image/3D Adoption (aivyx-coder) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `generate_image` and `generate_3d` tools to the existing
`aivyx-tools` crate, backed by the just-shipped
`aivyx-vision-core`/`aivyx-vision-mold` crates. This is `aivyx-coder`'s
adoption of Aivyx-Vision's Milestone 2 Pass A, alongside the already-shipped
`generate_svg` (Milestone 1).

**Architecture:** Both new tools live in a new file,
`crates/aivyx-tools/src/tools/generation_tools.rs`, and depend on
`Arc<dyn aivyx_vision_core::GenerationProvider>` (a trait object) —
mirroring `GenerateSvgTool`'s own `Arc<dyn TextCompleter>` dependency.
`agent_builder.rs` constructs one concrete `aivyx_vision_mold::MoldProvider`
and hands the same `Arc` to both tools. Unlike `generate_svg`
(`ActionKind::Network`, an auto-allow tier), `generate_image` is
`ActionKind::Write` — it has a real filesystem side effect (a new file
under `assets/generated/`), so it genuinely prompts for confirmation
(then is cacheable), matching `write_file`. The permission target is the
`assets/generated/` directory itself, not a specific filename — the real
output name is a backend-minted UUID the operator never picks or reviews,
so one approval covers the rest of the session. `reference_image` uses
the crate's existing, unrestricted `resolve(cwd, path)` helper — the same
one `read_file`/`write_file` already use — since every tool call here runs
in-process through one central `ConfirmationGate`, unlike `aivyx-pa`'s
adoption (a separate OS process with no sandbox visibility, which needed a
bespoke restriction this product's architecture doesn't). See
`docs/superpowers/specs/2026-09-18-vision-image-3d-adoption-design.md` for
the full reasoning behind every decision below.

**Tech Stack:** Rust, edition 2024. `aivyx-vision-core`/`aivyx-vision-mold`
(new external git dependencies, same repo/rev as the existing
`aivyx-vision-svg` pin), `async-trait`, `schemars`, `serde` (all already
workspace dependencies).

## Global Constraints

- `cargo clippy --workspace --all-targets` clean; `cargo test --workspace`
  green.
- `aivyx-vision-svg`, `aivyx-vision-core`, and `aivyx-vision-mold` are all
  pinned to the same rev, `caed4c0875a19ef5c7f4059ddda8cd6364f9fda4`
  (Milestone 2 Pass A's merge commit in the `aivyx-vision` repo) — keep
  them in lockstep, don't let the existing `aivyx-vision-svg` pin drift
  stale relative to the two new ones.
- `generate_image`/`generate_3d` are only registered when
  `settings.vision.enabled` — the exact same conditional-registration
  shape already used for `web_fetch`/`web_search`. `enabled = false`
  (the default) means zero behavior change for every existing install. If
  `enabled` is `true` but `MoldProvider::new(...)` fails to construct,
  the agent degrades gracefully (logs to stderr, starts with neither tool
  registered) rather than failing the whole process.
- `generate_image` is `ActionKind::Write`, `PermissionTarget::Path`
  pointing at the `assets/generated/` directory (not a specific future
  filename). Neither tool overrides `needs_checkpoint()` or
  `mutates_outside_session()` — both default to `true`/`true`, matching
  `write_file`'s own (absence of an) override, since a generated file is
  a real worktree mutation that deserves a checkpoint like any other.
- `reference_image` is resolved via the crate's existing
  `crate::path_resolve::resolve(cwd, path)` helper, with no additional
  restriction — never build a bespoke path sandbox for this one field.
- `aivyx_vision_core::FakeGenerationProvider` requires that crate's
  `testing` Cargo feature, which must be enabled under
  `[dev-dependencies]` only in `crates/aivyx-tools/Cargo.toml` — a real
  gap `aivyx-pa`'s own adoption's implementer found and fixed after the
  plan initially missed it; specified directly here so it doesn't need
  rediscovering.
- Re-read every file cited by line number below before editing — accurate
  as of this plan's own research (2026-09-18) but the repo moves.

---

## Task 1: Pin `aivyx-vision-core`/`aivyx-vision-mold`, bump `aivyx-vision-svg`

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/Cargo.toml` (root —
  `[workspace.dependencies]`, around the existing `aivyx-vision-svg` entry)
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-tools/Cargo.toml`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx/Cargo.toml`

**Interfaces:** none new — dependency wiring only. Produces:
`aivyx_vision_core::{GenerationProvider, ImageRequest, ThreeDRequest,
GeneratedAsset, VisionError}` importable from `aivyx-tools`; additionally
`aivyx_vision_mold::{MoldProvider, MoldConfig}` importable from the
`aivyx` binary crate (which constructs the concrete provider — `aivyx-tools`
itself never depends on `aivyx-vision-mold`, only the trait crate).

This is not a TDD task (no new behavior yet) — the steps replace
write-test/verify-fail with record-baseline/verify-no-regression.

- [ ] **Step 1: Record the baseline**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-tools 2>&1 | tail -20
cargo test -p aivyx 2>&1 | tail -20
```

Expected: all existing tests pass in both crates (record the counts).

- [ ] **Step 2: Bump the `aivyx-vision-svg` pin, add the two new pins**

In root `Cargo.toml`'s `[workspace.dependencies]` section, replace the
existing `aivyx-vision-svg` entry (currently at line 34, pinned to
`a80be4709b41382ef62c20545bb706e18a1d5ee3`) with:

```toml
# Not on crates.io -- pinned by commit SHA, same as aivyx-checkpoint/
# aivyx-confine/aivyx-injection-guard above. No platform-specific
# backend, so no target-gating needed. All three pinned to the same rev
# (Milestone 2 Pass A's merge commit in that repo) so they never drift
# out of lockstep with each other.
aivyx-vision-svg = { git = "https://github.com/Aivyx-Agent/aivyx-vision", rev = "caed4c0875a19ef5c7f4059ddda8cd6364f9fda4" }
aivyx-vision-core = { git = "https://github.com/Aivyx-Agent/aivyx-vision", rev = "caed4c0875a19ef5c7f4059ddda8cd6364f9fda4" }
aivyx-vision-mold = { git = "https://github.com/Aivyx-Agent/aivyx-vision", rev = "caed4c0875a19ef5c7f4059ddda8cd6364f9fda4" }
```

- [ ] **Step 3: Add `aivyx-vision-core` to `aivyx-tools`**

In `crates/aivyx-tools/Cargo.toml`, immediately after the existing
`aivyx-vision-svg = { workspace = true }` line (currently line 13), add:

```toml
aivyx-vision-core = { workspace = true }
```

And add to `[dev-dependencies]` (currently just `tempfile = "3.27.0"` at
line 34):

```toml
# aivyx-vision-core's `FakeGenerationProvider` test double is gated behind
# its own `testing` Cargo feature (`#[cfg(any(test, feature = "testing"))]`
# in that crate -- `test` there only applies to aivyx-vision-core's own
# `cargo test`, not to this crate's). Listed here, in [dev-dependencies]
# only, so the feature is enabled for this crate's own test builds
# without leaking into normal (non-test) builds of aivyx-tools or
# anything that depends on it.
aivyx-vision-core = { workspace = true, features = ["testing"] }
```

- [ ] **Step 4: Add `aivyx-vision-core`/`aivyx-vision-mold` to the `aivyx` binary crate**

In `crates/aivyx/Cargo.toml`, immediately after the existing
`aivyx-vision-svg = { workspace = true }` line (currently line 23), add:

```toml
aivyx-vision-core = { workspace = true }
aivyx-vision-mold = { workspace = true }
```

(`agent_builder.rs`, Task 4, constructs the concrete `MoldProvider`
directly and also needs the `Arc<dyn GenerationProvider>` type — the same
reason it already depends directly on `aivyx-vision-svg` for
`Arc<dyn aivyx_vision_svg::TextCompleter>`, not just transitively through
`aivyx-tools`.)

- [ ] **Step 5: Build and verify no regression**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build -p aivyx-tools -p aivyx
cargo test -p aivyx-tools 2>&1 | tail -20
cargo test -p aivyx 2>&1 | tail -20
cargo clippy -p aivyx-tools -p aivyx --all-targets -- -D warnings
cargo fmt --check
```

Expected: builds clean (pulling the two new git deps), identical test
counts/pass rates to Step 1's baselines, clippy/fmt clean.

- [ ] **Step 6: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add Cargo.toml Cargo.lock crates/aivyx-tools/Cargo.toml crates/aivyx/Cargo.toml
git commit -m "chore: pin aivyx-vision-core/aivyx-vision-mold, bump aivyx-vision-svg to the same rev

All three Aivyx-Vision crates now pinned to caed4c0 (Milestone 2 Pass A's
merge commit), keeping them in lockstep. aivyx-tools gets aivyx-vision-core
only (the trait crate, plus its testing feature as a dev-dependency);
aivyx-vision-mold is a direct dependency of the aivyx binary crate only,
which is the sole place that constructs a concrete MoldProvider. No
behavior change yet -- this is dependency wiring ahead of Task 3's tool
code."
```

---

## Task 2: `VisionSettings` config

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-config/src/lib.rs`

**Interfaces:**
- Produces: `VisionSettings { enabled: bool, broker_url: String, mold_url: String, api_key: Option<String> }`,
  `Settings.vision: VisionSettings`.

- [ ] **Step 1: Write the failing tests**

Find `aivyx-config`'s existing test module (search for `mod tests` in
`lib.rs`) and add:

```rust
#[test]
fn vision_settings_default_is_disabled_with_sane_endpoints() {
    let settings = Settings::default();
    assert!(!settings.vision.enabled);
    assert_eq!(settings.vision.broker_url, "http://127.0.0.1:8899");
    assert_eq!(settings.vision.mold_url, "http://127.0.0.1:7680");
    assert_eq!(settings.vision.api_key, None);
}

#[test]
fn vision_settings_parses_from_a_full_toml_section() {
    let toml_str = r#"
        [vision]
        enabled = true
        broker_url = "http://127.0.0.1:9999"
        mold_url = "http://127.0.0.1:8888"
        api_key = "secret"
    "#;
    let settings: Settings = toml::from_str(toml_str).unwrap();
    assert!(settings.vision.enabled);
    assert_eq!(settings.vision.broker_url, "http://127.0.0.1:9999");
    assert_eq!(settings.vision.mold_url, "http://127.0.0.1:8888");
    assert_eq!(settings.vision.api_key.as_deref(), Some("secret"));
}

#[test]
fn vision_settings_defaults_partial_fields_when_only_enabled_is_set() {
    let toml_str = r#"
        [vision]
        enabled = true
    "#;
    let settings: Settings = toml::from_str(toml_str).unwrap();
    assert!(settings.vision.enabled);
    assert_eq!(settings.vision.broker_url, "http://127.0.0.1:8899");
    assert_eq!(settings.vision.mold_url, "http://127.0.0.1:7680");
}

#[test]
fn settings_with_no_vision_section_at_all_still_parses() {
    let settings: Settings = toml::from_str("").unwrap();
    assert!(!settings.vision.enabled);
}
```

(Check the exact style of the nearest existing `WebSettings`-parsing test
in this module first and match its precise TOML-string/assertion idiom if
it differs in a superficial way from the sketch above — the assertions
matter more than the exact literal formatting.)

- [ ] **Step 2: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-config vision_settings -- --nocapture
```

Expected: compile error — `Settings` has no `vision` field, `VisionSettings`
doesn't exist yet.

- [ ] **Step 3: Implement**

Add `pub vision: VisionSettings,` to `Settings` (currently lines 34-54),
immediately after the existing `pub web: WebSettings,` line (line 50):

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub backend: BackendSettings,
    pub permissions: PermissionSettings,
    pub sandbox: SandboxSettings,
    pub git: GitSettings,
    pub repo_map: RepoMapSettings,
    pub council: CouncilSettings,
    pub architect: ArchitectSettings,
    pub verification: VerificationSettings,
    pub autonomous: AutonomousSettings,
    pub sub_agent: SubAgentSettings,
    pub mcp_server: McpServerSettings,
    pub lsp: LspSettings,
    pub agents_file: AgentsFileSettings,
    pub editor_context: EditorContextSettings,
    pub editor_approval: EditorApprovalSettings,
    pub web: WebSettings,
    pub vision: VisionSettings,
    pub mcp: McpSettings,
    pub persona: PersonaSettings,
    pub repl: ReplSettings,
}
```

Add a new struct + `Default` impl near `WebSettings` (currently at line
298-312), mirroring its exact derive/`#[serde(default)]` shape:

```rust
/// Governs `generate_image`/`generate_3d` (`aivyx-tools`'s
/// `GenerateImageTool`/`GenerateThreeDTool`) -- both registered only when
/// `enabled`, since they depend on external infrastructure
/// (`aivyx-broker`, `mold serve`) that isn't installed by default,
/// unlike `generate_svg` which reuses the agent's own already-configured
/// LLM backend and needs no separate opt-in. See
/// `docs/superpowers/specs/2026-09-18-vision-image-3d-adoption-design.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VisionSettings {
    pub enabled: bool,
    pub broker_url: String,
    pub mold_url: String,
    pub api_key: Option<String>,
}

impl Default for VisionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            broker_url: "http://127.0.0.1:8899".to_string(),
            mold_url: "http://127.0.0.1:7680".to_string(),
            api_key: None,
        }
    }
}
```

- [ ] **Step 4: Run to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-config vision_settings -- --nocapture
```

Expected: all 4 new tests pass, plus every pre-existing `aivyx-config`
test still passes (none of them set `[vision]`, so they should all still
get the disabled-by-default `VisionSettings`).

- [ ] **Step 5: Run full crate check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-config
cargo clippy -p aivyx-config --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx-config/src/lib.rs
git commit -m "feat: add VisionSettings config for generate_image/generate_3d

Mirrors WebSettings' exact shape and Default idiom: an enabled flag
(default false -- opt-in, needs external infra) plus broker_url/mold_url/
api_key. Settings' own struct-level #[serde(default)] means an install
with no [vision] section at all still parses, disabled -- zero behavior
change for every existing config.toml."
```

---

## Task 3: `GenerateImageTool` + `GenerateThreeDTool`

**Files:**
- Create: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-tools/src/tools/generation_tools.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-tools/src/tools/mod.rs`

**Interfaces:**
- Consumes: `aivyx_vision_core::{GenerationProvider, ImageRequest,
  ThreeDRequest, GeneratedAsset, VisionError}` (Task 1's new dependency),
  `crate::path_resolve::resolve` (existing, already `pub(crate)`),
  `crate::{Tool, ToolError, ToolExecutionContext}` (existing).
- Produces: `GenerateImageTool::new(provider: Arc<dyn GenerationProvider>) -> Self`,
  `GenerateThreeDTool::new(provider: Arc<dyn GenerationProvider>) -> Self`,
  both implementing `Tool`.

- [ ] **Step 1: Write the failing tests**

```rust
use std::sync::Arc;

use aivyx_types::ToolOutput;
use aivyx_vision_core::{FakeGenerationProvider, GeneratedAsset, VisionError};
use serde_json::json;

use super::*;

fn sample_asset(path: &str) -> GeneratedAsset {
    GeneratedAsset {
        path: path.into(),
        backend: "mold".to_string(),
        seed_used: Some(42),
        generated_at: std::time::SystemTime::now(),
    }
}

fn ctx() -> ToolExecutionContext {
    ToolExecutionContext {
        cwd: std::env::temp_dir(),
        confiner: Arc::new(aivyx_sandbox::NoopConfiner),
        cancellation: tokio_util::sync::CancellationToken::new(),
    }
}

#[test]
fn generate_image_metadata_is_sound() {
    let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
    let tool = GenerateImageTool::new(Arc::new(provider));
    assert_eq!(tool.name(), "generate_image");
    let request = tool
        .permission_request(&json!({"prompt": "a red circle"}), &ctx().cwd)
        .unwrap();
    assert_eq!(request.action, ActionKind::Write);
    assert_eq!(
        request.target,
        PermissionTarget::Path(ctx().cwd.join("assets/generated"))
    );
}

#[test]
fn generate_image_needs_checkpoint_and_mutates_outside_session_default_true() {
    // Unlike generate_svg's deliberate false override, generate_image
    // has a real filesystem effect and must keep both defaults.
    let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
    let tool = GenerateImageTool::new(Arc::new(provider));
    assert!(tool.mutates_outside_session());
    assert!(tool.needs_checkpoint());
}

#[tokio::test]
async fn generate_image_returns_the_generated_asset_on_success() {
    let provider =
        FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/out/abc.png")));
    let tool = GenerateImageTool::new(Arc::new(provider));
    let output = tool
        .execute(json!({"prompt": "a red circle"}), &ctx())
        .await
        .unwrap();
    let ToolOutput::Ok(msg) = output else {
        panic!("expected Ok output, got {output:?}")
    };
    assert!(msg.contains("/tmp/out/abc.png"));
    assert!(msg.contains("42"));
}

#[tokio::test]
async fn generate_image_passes_width_height_seed_style_hint_through() {
    let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
    let provider = Arc::new(provider);
    let tool = GenerateImageTool::new(provider.clone());
    tool.execute(
        json!({
            "prompt": "a mountain",
            "width": 512,
            "height": 768,
            "seed": 7,
            "style_hint": "watercolor"
        }),
        &ctx(),
    )
    .await
    .unwrap();
    let captured = provider
        .captured_image_request()
        .expect("must capture the request");
    assert_eq!(captured.prompt, "a mountain");
    assert_eq!(captured.width, Some(512));
    assert_eq!(captured.height, Some(768));
    assert_eq!(captured.seed, Some(7));
    assert_eq!(captured.style_hint.as_deref(), Some("watercolor"));
}

#[tokio::test]
async fn generate_image_fails_clearly_on_a_missing_prompt_field() {
    let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
    let tool = GenerateImageTool::new(Arc::new(provider));
    let result = tool.execute(json!({}), &ctx()).await;
    assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
}

#[tokio::test]
async fn generate_image_fails_clearly_when_generation_errors() {
    let provider = FakeGenerationProvider::with_image_result(Err(VisionError::GpuLockTimeout));
    let tool = GenerateImageTool::new(Arc::new(provider));
    let output = tool
        .execute(json!({"prompt": "anything"}), &ctx())
        .await
        .unwrap();
    let ToolOutput::Error(msg) = output else {
        panic!("expected Error output, got {output:?}")
    };
    assert!(msg.contains("generate_image"));
}

#[tokio::test]
async fn generate_image_resolves_a_reference_image_relative_to_cwd() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ref.png"), b"fake png bytes").unwrap();
    let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
    let provider = Arc::new(provider);
    let tool = GenerateImageTool::new(provider.clone());
    let ctx = ToolExecutionContext {
        cwd: dir.path().to_path_buf(),
        confiner: Arc::new(aivyx_sandbox::NoopConfiner),
        cancellation: tokio_util::sync::CancellationToken::new(),
    };
    tool.execute(
        json!({"prompt": "edit this", "reference_image": "ref.png"}),
        &ctx,
    )
    .await
    .unwrap();
    let captured = provider.captured_image_request().unwrap();
    assert_eq!(
        captured.reference_image,
        Some(crate::path_resolve::resolve(dir.path(), "ref.png"))
    );
}

#[test]
fn generate_3d_metadata_is_sound() {
    let provider = FakeGenerationProvider::with_3d_result(Ok(sample_asset("/tmp/x.glb")));
    let tool = GenerateThreeDTool::new(Arc::new(provider));
    assert_eq!(tool.name(), "generate_3d");
    let request = tool
        .permission_request(&json!({"prompt": "a small statue"}), &ctx().cwd)
        .unwrap();
    assert_eq!(request.action, ActionKind::Write);
}

#[tokio::test]
async fn generate_3d_returns_the_generated_asset_on_success() {
    let provider = FakeGenerationProvider::with_3d_result(Ok(sample_asset("/tmp/out/x.glb")));
    let tool = GenerateThreeDTool::new(Arc::new(provider));
    let output = tool
        .execute(json!({"prompt": "a small statue"}), &ctx())
        .await
        .unwrap();
    let ToolOutput::Ok(msg) = output else {
        panic!("expected Ok output, got {output:?}")
    };
    assert!(msg.contains("/tmp/out/x.glb"));
}

#[tokio::test]
async fn generate_3d_fails_clearly_when_unsupported() {
    let provider = FakeGenerationProvider::with_3d_result(Err(VisionError::Unsupported(
        "3D generation via mold (Pass B is not yet implemented)",
    )));
    let tool = GenerateThreeDTool::new(Arc::new(provider));
    let output = tool
        .execute(json!({"prompt": "a small statue"}), &ctx())
        .await
        .unwrap();
    assert!(matches!(output, ToolOutput::Error(_)));
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-tools generation_tools:: -- --nocapture
```

Expected: compile error — `generation_tools` module, `GenerateImageTool`,
`GenerateThreeDTool` don't exist yet.

- [ ] **Step 3: Implement `generation_tools.rs`**

```rust
//! `generate_image` and `generate_3d` -- both backed by the same
//! `Arc<dyn GenerationProvider>` (concretely `aivyx-vision-mold`'s
//! `MoldProvider` today; see `agent_builder.rs`). `generate_3d` always
//! fails today with `VisionError::Unsupported` -- mold's Pass B (async 3D
//! generation) isn't built yet -- but the tool ships now so it's
//! discoverable, and it starts working with zero changes here once a real
//! backend lands. See `docs/superpowers/specs/
//! 2026-09-18-vision-image-3d-adoption-design.md`.
//!
//! Unlike `generate_svg` (`ActionKind::Network`, an auto-allow tier here),
//! both tools in this file are `ActionKind::Write`: they write a real file
//! into the project (`assets/generated/<uuid>.<ext>`), which is the thing
//! that actually needs operator confirmation, not the network call that
//! produces its content. The permission target is the `assets/generated/`
//! directory itself, not the specific (backend-minted, never
//! operator-chosen) filename -- see this crate's own design doc for why
//! that's the correct Always-Allow cache key here, not a shortcut.

use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use aivyx_vision_core::{GeneratedAsset, GenerationProvider, ImageRequest, ThreeDRequest};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::path_resolve::resolve;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct GenerateImageArgs {
    /// Description of the image to generate, e.g. "a small red circle icon".
    prompt: String,
    width: Option<u32>,
    height: Option<u32>,
    seed: Option<u64>,
    /// A free-text style nudge, e.g. "watercolor", "pixel art".
    style_hint: Option<String>,
    /// Path to a reference image for image-to-image generation, absolute or relative to the working directory.
    reference_image: Option<String>,
}

pub struct GenerateImageTool {
    provider: Arc<dyn GenerationProvider>,
}

impl GenerateImageTool {
    pub fn new(provider: Arc<dyn GenerationProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for GenerateImageTool {
    fn name(&self) -> &str {
        "generate_image"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Generate an image from a text prompt via a local image-generation \
                backend and save it under assets/generated/. Returns the saved file's path. \
                `reference_image`, if given, enables image-to-image generation."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GenerateImageArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GenerateImageArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(cwd.join("assets/generated")),
            arguments_preview: json!({ "prompt": args.prompt }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GenerateImageArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let reference_image = args
            .reference_image
            .as_ref()
            .map(|path| resolve(&ctx.cwd, path));
        let req = ImageRequest {
            prompt: args.prompt,
            reference_image,
            width: args.width,
            height: args.height,
            seed: args.seed,
            style_hint: args.style_hint,
        };
        match self.provider.generate_image(req).await {
            Ok(asset) => Ok(ToolOutput::Ok(success_message("image", &asset))),
            Err(e) => Ok(ToolOutput::Error(format!("generate_image: {e}"))),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct GenerateThreeDArgs {
    /// Description of the 3D model to generate.
    prompt: String,
}

pub struct GenerateThreeDTool {
    provider: Arc<dyn GenerationProvider>,
}

impl GenerateThreeDTool {
    pub fn new(provider: Arc<dyn GenerationProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for GenerateThreeDTool {
    fn name(&self) -> &str {
        "generate_3d"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Generate a 3D model from a text prompt and save it under \
                assets/generated/. **Not yet implemented** -- every call currently fails \
                with a clear error; the tool exists now so it's discoverable ahead of a \
                future backend that implements it."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GenerateThreeDArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GenerateThreeDArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(cwd.join("assets/generated")),
            arguments_preview: json!({ "prompt": args.prompt }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GenerateThreeDArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let req = ThreeDRequest {
            prompt: args.prompt,
            reference_image: None,
            style_hint: None,
        };
        match self.provider.generate_3d(req).await {
            Ok(asset) => Ok(ToolOutput::Ok(success_message("3D model", &asset))),
            Err(e) => Ok(ToolOutput::Error(format!("generate_3d: {e}"))),
        }
    }
}

fn success_message(kind: &str, asset: &GeneratedAsset) -> String {
    let seed_display = asset
        .seed_used
        .map(|s| s.to_string())
        .unwrap_or_else(|| "none".to_string());
    format!(
        "generated {kind} saved to {} (seed {seed_display})",
        asset.path.display()
    )
}

#[cfg(test)]
mod tests {
    // (Step 1's tests land here.)
}
```

Save as `crates/aivyx-tools/src/tools/generation_tools.rs`.

- [ ] **Step 4: Wire the module into `mod.rs`**

In `crates/aivyx-tools/src/tools/mod.rs`, insert `mod generation_tools;`
alphabetically between the existing `mod generate_svg_completer;` (line 5)
and `mod git_branch;` (line 6):

```rust
mod delete_file;
mod edit_file;
mod find_references;
mod generate_svg;
mod generate_svg_completer;
mod generation_tools;
mod git_branch;
```

And insert the corresponding `pub use` line alphabetically between the
existing `pub use generate_svg_completer::CoderTextCompleter;` (line 35)
and `pub use git_branch::GitBranchTool;` (line 36):

```rust
pub use generate_svg::GenerateSvgTool;
pub use generate_svg_completer::CoderTextCompleter;
pub use generation_tools::{GenerateImageTool, GenerateThreeDTool};
pub use git_branch::GitBranchTool;
```

- [ ] **Step 5: Run to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-tools -- --nocapture
```

Expected: all 10 new tests pass, plus every pre-existing `aivyx-tools`
test still green.

- [ ] **Step 6: Run full crate check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-tools
cargo clippy -p aivyx-tools --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 7: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx-tools/src/tools/generation_tools.rs crates/aivyx-tools/src/tools/mod.rs
git commit -m "feat: add GenerateImageTool + GenerateThreeDTool

Both depend on Arc<dyn GenerationProvider>, mirroring GenerateSvgTool's
own Arc<dyn TextCompleter> dependency -- wired to a concrete MoldProvider
in agent_builder.rs (Task 4). ActionKind::Write (not Network, unlike
generate_svg) since these tools have a real filesystem side effect;
permission target is the assets/generated/ directory itself, not a
specific filename, since the actual output name is a backend-minted UUID
never chosen or reviewed by the operator. reference_image resolves via
the crate's existing, unrestricted resolve(cwd, path) helper -- the same
one read_file/write_file already use. generate_3d always fails with
VisionError::Unsupported today -- ships now so it's discoverable, starts
working once a real 3D backend lands with no changes here."
```

---

## Task 4: Wire into `agent_builder.rs`, update docs, final verification

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx/src/agent_builder.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/README.md`

**Interfaces:** none new — wiring + documentation only.

- [ ] **Step 1: Add the import**

In `crates/aivyx/src/agent_builder.rs`'s existing `use aivyx_tools::{ ... };`
block (currently lines 24-33), add `GenerateImageTool` and
`GenerateThreeDTool` in alphabetical position (between `FindReferencesTool`
and `GenerateSvgTool`... check exact alphabetical placement against the
real current list — `GenerateImageTool`/`GenerateThreeDTool` sort between
`FindReferencesTool` and `GenerateSvgTool`... re-verify directly: `G-e-n-e-r-a-t-e-I` vs
`G-e-n-e-r-a-t-e-S` — `I` < `S`, so `GenerateImageTool`/`GenerateThreeDTool`
sort *before* `GenerateSvgTool`):

```rust
use aivyx_tools::{
    CoderTextCompleter, CommandSpec, DeleteFileTool, EditFileTool, FindReferencesTool,
    GenerateImageTool, GenerateSvgTool, GenerateThreeDTool, GetMcpPromptTool, GitBranchTool,
    GitCheckpointer, GitCommitTool, GitPrTool, GitPushTool, GitReadTool, GlobTool,
    GoToDefinitionTool, GrepTool, ListMcpPromptsTool, ListMcpResourcesTool, LspClient, McpClient,
    McpToolAdapter, MemoryForgetTool, MemoryReadTool, MemoryWriteTool, MoveFileTool, PatchFileTool,
    ReadFileTool, ReadMcpResourceTool, RememberPreferenceTool, ReplResizeTarget, ReplSendTool,
    ReplStartTool, ReplStopTool, RunCommandTool, RunShellTool, SetTasksTool, ToolExecutor,
    ToolRegistry, WebFetchTool, WebSearchTool, WriteFileTool, new_shared_repl_session,
};
```

Also add `use aivyx_vision_core::GenerationProvider;` near the existing
`use aivyx_vision_svg` reference (there's no standalone `use` for it today
— `aivyx_vision_svg::TextCompleter` is referenced with its full path
inline at the `vision_completer` binding; follow that same inline-path
style for `aivyx_vision_core`/`aivyx_vision_mold` too in Step 2 below,
rather than adding new `use` lines, for consistency with how
`aivyx_vision_svg` is already referenced in this exact file).

- [ ] **Step 2: Register both tools**

Immediately after the existing
`registry.register(Arc::new(GenerateSvgTool::new(vision_completer)));`
line (currently line 429, right before the `// Every configured server
connects concurrently...` MCP-discovery comment), insert:

```rust
    // Unlike generate_svg above, generate_image/generate_3d genuinely
    // need external infrastructure (aivyx-broker, mold serve) that isn't
    // installed by default -- gated the same way web_fetch/web_search
    // are, not always-registered like generate_svg.
    if settings.vision.enabled {
        let mold_config = aivyx_vision_mold::MoldConfig {
            broker_url: settings.vision.broker_url.clone(),
            mold_url: settings.vision.mold_url.clone(),
            api_key: settings.vision.api_key.clone(),
            output_dir: cwd.join("assets/generated"),
        };
        match aivyx_vision_mold::MoldProvider::new(mold_config) {
            Ok(provider) => {
                let provider: Arc<dyn aivyx_vision_core::GenerationProvider> = Arc::new(provider);
                registry.register(Arc::new(GenerateImageTool::new(provider.clone())));
                registry.register(Arc::new(GenerateThreeDTool::new(provider)));
            }
            Err(e) => {
                eprintln!(
                    "aivyx-coder: failed to construct the mold provider, \
                     generate_image/generate_3d will be unavailable: {e}"
                );
            }
        }
    }
```

- [ ] **Step 3: Build and manual smoke check (no `[vision]` configured)**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build -p aivyx
```

Expected: builds clean. (A full runtime smoke test needs real `mold serve`
+ `aivyx-broker` instances, out of scope for this step — the point here is
confirming the new code compiles and the `if settings.vision.enabled`
branch is inert when unset, which Task 2's own tests already cover at the
`aivyx-config` level.)

- [ ] **Step 4: Update `README.md`**

Add two new rows to the existing "Tools" table (currently at line 1157),
immediately after the existing `generate_svg` row:

```markdown
| `generate_image` | generate an image from a text prompt via a local backend, saved under `assets/generated/` | prompt (then cacheable) |
| `generate_3d` | generate a 3D model from a text prompt -- **not yet implemented**, always fails today | prompt (then cacheable) |
```

Add a new prose section immediately after `generate_svg`'s own section
(currently ending around line 548, right before the "## Tools" heading),
following that section's exact structure:

```markdown
**`generate_image(prompt, width?, height?, seed?, style_hint?, reference_image?)`**:
generates an image via a local `mold serve` instance (coordinated with
local LLM inference through `aivyx-broker`'s GPU lock, so it doesn't
contend with this same conversation's own backend calls), saved under
`assets/generated/<uuid>.<ext>` in the current project. `reference_image`,
if given, is resolved the same way `read_file`/`write_file` resolve their
own `path` argument (absolute or cwd-relative) -- no additional
restriction. Classified `ActionKind::Write` (unlike `generate_svg`'s
`Network` tier): it has a real filesystem effect, so it prompts for
confirmation like `write_file`, then is cacheable. The permission target
is the `assets/generated/` directory itself, not the specific (randomly
named) output file -- one approval covers the rest of the session, since
the actual filename is never something you'd choose or review ahead of
time. Requires `[vision] enabled = true` (default `false`) plus
`broker_url`/`mold_url` pointing at a running `aivyx-broker` and
`mold serve` -- see "Configuration" below.

**`generate_3d(prompt)`**: same backend and permission shape as
`generate_image` above. **Not yet implemented** -- every call currently
fails with a clear error (mold's async 3D generation isn't built yet);
registered now so it's discoverable ahead of a future backend.
```

(No change needed to the "## Configuration reference" section, currently
lines 1386-1507 — confirmed directly: that section's single big TOML
example does not actually include `[web]`, `[mcp_server]`, or several
other real config sections either; `[web]`'s fields are documented purely
in prose within the Tools section, exactly where the prose above
documents `[vision]`'s fields (`enabled`, `broker_url`, `mold_url`,
`api_key`, matching `VisionSettings::default()`'s values verbatim). Adding
a `[vision]` block to "Configuration reference" would be introducing a
level of documentation `[web]` itself doesn't have — inconsistent with
the file's own real convention, not an omission to fix.)

- [ ] **Step 5: Final full-workspace verification**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace 2>&1 | tail -40
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: all green.

- [ ] **Step 6: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx/src/agent_builder.rs README.md
git commit -m "feat: wire generate_image/generate_3d into agent_builder.rs, update docs

Registers both tools only when [vision] enabled = true, degrading
gracefully (agent still starts, just without them) if it's unset or the
provider fails to construct. Completes aivyx-coder's adoption of
Aivyx-Vision Milestone 2 Pass A."
```

---

## Final verification (after all 4 tasks land)

- [ ] Run the complete workspace test suite once, not per-task:

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test --workspace 2>&1 | tail -40
```

- [ ] Run the documented clippy + fmt commands once more:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] This plan does not decide whether to push a branch / open a PR —
  follow `superpowers:finishing-a-development-branch` once all tasks are
  individually reviewed and a final whole-branch review has passed, same
  as every other plan executed this cycle.

## Explicitly out of scope for this plan

(Copied forward from the design spec's own "What this design does not
decide" section, so a future reader doesn't mistake this plan's silence
on these for an oversight.)

- Pass B itself (a real `generate_3d` implementation) — separate,
  not-yet-scoped future work in the `aivyx-vision` repo.
- `aivyx-vision-comfyui` — deferred entirely per the ecosystem spec's own
  2026-09-18 amendment.
- Any budget/rate-limiting integration — `aivyx-coder` has no equivalent
  of `aivyx-pa`'s Chapter K cost-governance crate; not applicable here.
- Retention/cleanup of accumulated generated files under
  `assets/generated/` — operator-managed, matching the ecosystem spec's
  own v1 default.
- The GPU-lock async-cancellation lease-leak gap noted in
  `aivyx-vision-mold`'s own final review — a pre-existing gap in the
  engine crate this adoption depends on, not something this plan can fix.
