# Vision SVG Adoption (aivyx-coder) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire `aivyx-vision-svg`'s `generate_svg` into `aivyx-coder` as a
real, agent-invokable tool — `generate_svg` — added directly to the
existing `aivyx-tools` crate. This is `aivyx-coder`'s adoption half of
Aivyx-Vision's Milestone 1 (the other half, `aivyx-pa`'s adoption, already
shipped in that repo as a separate tool-process crate, `aivyx-vision`).

**Architecture — genuinely simpler than `aivyx-pa`'s adoption, for a real
reason:** `aivyx-pa`'s tools run as separate tool-*process* binaries (IPC
over a subprocess boundary), so that adoption needed its own,
separately-configured LLM connection. `aivyx-coder`'s tools are plain
Rust structs compiled directly into the same process as the agent's own
turn loop — there is no subprocess boundary here at all. This means the
new tool can hold the **exact same** `Arc<dyn LlmBackend>` instance
`agent_builder.rs` already builds for the agent's own conversation —
zero new config, zero duplicate provider setup, zero drift risk between
"what model answers my prompts" and "what model draws my icons." A small
adapter, `CoderTextCompleter`, implements `aivyx-vision-svg`'s
`TextCompleter` trait by wrapping that shared backend.

**Classification, grounded in the real precedent (`web_search`/
`web_fetch`, the two existing tools that reach outside the local
session):** `ActionKind::Network` (an LLM completion call is exactly the
"reaches outside the local session/filesystem" case this variant exists
for), `mutates_outside_session()` left at the trait's fail-closed default
`true` (hidden from plan mode, same as `web_search`/`web_fetch` despite
neither mutating anything either — network reach itself is what the
default protects), `needs_checkpoint()` overridden to `false` (no
filesystem mutation, nothing for a checkpoint to protect — identical
override and rationale to `web_search`'s own).

**Output shape, deliberately kept minimal and consistent with the
`aivyx-pa` side:** returns the sanitized SVG markup as plain text via
`ToolOutput::Ok(svg)` — it does **not** write a file itself. `aivyx-pa`'s
own `vision.generate_svg` tool (already shipped) does the same: returns
`{"svg": "..."}`, lets the caller decide whether/where to persist it. For
`aivyx-coder` specifically, the model already has `write_file` for that —
adding a second, tool-owned file-writing path here would duplicate an
existing, already-gated capability for no real benefit, and would keep
the two products' tool shapes needlessly asymmetric.

**Injection scanning**: confirmed already applied centrally, not
something this tool needs to opt into — `aivyx-core/src/delegate.rs`
calls `scan_for_injection_markers` on tool output text as part of the
turn loop itself (CLAUDE.md: "a heuristic scan... always runs, in every
mode"), so `generate_svg`'s output is covered automatically the same way
every other tool's is.

**Tech Stack:** Rust, edition matching this workspace. `aivyx-tools`
(existing crate — no new crate created), new dependencies added to it:
`aivyx-llm` (path, intra-workspace — not previously a dependency of
`aivyx-tools`, since no tool has ever needed to call the LLM before) and
`aivyx-vision-svg` (new external git dependency, pinned by commit SHA
like `aivyx-checkpoint`/`aivyx-confine`/`aivyx-injection-guard`).

## Global Constraints

- `cargo clippy --workspace --all-targets` must stay clean (this repo's
  own documented gate — unlike `aivyx-pa`, `aivyx-coder`'s CLAUDE.md
  names `--workspace`, not a `default-members` carve-out; confirm this is
  still accurate before running the final check).
- `cargo test --workspace` must stay green.
- **"All tool calls go through the gate" is enforced by convention, not
  the type system** — this tool must not introduce any call path to
  `generate_svg`'s logic other than through `ToolExecutor::dispatch`.
- Re-read every file cited by line number below before editing — this
  plan's research is accurate as of 2026-09-18 but the repo moves.
- Match `web_search.rs`'s real, current shape exactly for anything this
  plan doesn't spell out in full (doc comment style, test structure,
  `needs_checkpoint` override wording) — it is this task's closest and
  most relevant sibling.

---

## Task 1: Add dependencies + the `CoderTextCompleter` adapter

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/Cargo.toml` (workspace
  root — add the `aivyx-vision-svg` pinned git dependency to
  `[workspace.dependencies]`)
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-tools/Cargo.toml`
  (add `aivyx-llm` as a path dependency, `aivyx-vision-svg` as a workspace
  dependency)
- Create: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-tools/src/tools/generate_svg_completer.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-tools/src/tools/mod.rs`
  (add the new module)

**Interfaces:**
- Consumes: `aivyx_llm::backend::{LlmBackend, ChatRequest, ToolChoice}`,
  `aivyx_types::{Message, Role}`, `aivyx_vision_svg::{TextCompleter,
  TextCompleterError}`.
- Produces: `CoderTextCompleter::new(backend: Arc<dyn LlmBackend>,
  max_tokens: u32) -> Self` implementing `TextCompleter`. Note this
  adapter does **not** take a separate `model` parameter the way
  `aivyx-pa`'s equivalent does — `LlmBackend::model_id()` already carries
  the configured model inside the backend instance itself, and
  `ChatRequest` has no separate model field to populate (confirm this by
  reading `LlmBackend`'s real trait definition in Step 1 before assuming
  it).

- [ ] **Step 1: Read the real, current API surface**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
grep -n "pub trait LlmBackend" -A 20 crates/aivyx-llm/src/backend.rs
grep -n "pub struct ChatRequest" -A 25 crates/aivyx-llm/src/backend.rs
grep -n "pub enum StreamEvent" -A 15 crates/aivyx-llm/src/backend.rs
grep -n "pub enum LlmError" -A 15 crates/aivyx-llm/src/backend.rs
grep -n "pub struct Message" -A 15 -B 2 crates/aivyx-types/src/lib.rs
```

Confirm: `LlmBackend::stream_chat(&self, request: ChatRequest) ->
Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError>`,
`ChatRequest { messages, tools, tool_choice, temperature, max_tokens,
id_slot, slot_hint }`, `StreamEvent::TextDelta(String)` as the text-chunk
variant, `Message::text(Role::User, prompt)` as the convenience
constructor. Adjust the code below if any of these have drifted.

- [ ] **Step 2: Get the current real HEAD of `aivyx-vision`'s `main` branch**

```bash
git ls-remote https://github.com/Aivyx-Agent/aivyx-vision main
```

Use the SHA this prints (not a hardcoded value) in Step 3.

- [ ] **Step 3: Add the `aivyx-vision-svg` pin to the workspace root `Cargo.toml`**

Insert alongside the existing `aivyx-confine`/`aivyx-checkpoint`/
`aivyx-injection-guard` entries in `[workspace.dependencies]`:

```toml
# Not on crates.io — pinned by commit SHA, same as aivyx-checkpoint/
# aivyx-confine/aivyx-injection-guard above. No platform-specific
# backend, so no target-gating needed.
aivyx-vision-svg = { git = "https://github.com/Aivyx-Agent/aivyx-vision", rev = "<the real SHA from Step 2>" }
```

- [ ] **Step 4: Add dependencies to `crates/aivyx-tools/Cargo.toml`**

```toml
aivyx-llm = { version = "0.1.0", path = "../aivyx-llm" }
aivyx-vision-svg = { workspace = true }
```

(Match the exact `version = "0.1.0"` convention this file already uses
for `aivyx-sandbox`/`aivyx-types` — confirm `aivyx-llm`'s real current
version in its own `Cargo.toml` first and use that value, not a guess.)

- [ ] **Step 5: Write the failing test**

Create `crates/aivyx-tools/src/tools/generate_svg_completer.rs`:

```rust
//! Adapts `aivyx-llm`'s `LlmBackend` (the same instance the agent's own
//! turn loop uses — no separate, duplicate LLM connection) to
//! `aivyx-vision-svg`'s `TextCompleter` seam.

use std::sync::Arc;

use aivyx_llm::backend::{ChatRequest, LlmBackend, StreamEvent, ToolChoice};
use aivyx_types::{Message, Role};
use aivyx_vision_svg::{TextCompleter, TextCompleterError};
use futures::StreamExt;

pub struct CoderTextCompleter {
    backend: Arc<dyn LlmBackend>,
    max_tokens: u32,
}

impl CoderTextCompleter {
    pub fn new(backend: Arc<dyn LlmBackend>, max_tokens: u32) -> Self {
        Self { backend, max_tokens }
    }
}

#[async_trait::async_trait]
impl TextCompleter for CoderTextCompleter {
    async fn complete(&self, prompt: &str) -> Result<String, TextCompleterError> {
        let request = ChatRequest {
            messages: vec![Message::text(Role::User, prompt)],
            tools: vec![],
            tool_choice: ToolChoice::None,
            temperature: None,
            max_tokens: Some(self.max_tokens),
            id_slot: None,
            slot_hint: None,
        };
        let mut stream = self
            .backend
            .stream_chat(request)
            .await
            .map_err(|e| TextCompleterError(e.to_string()))?;

        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let StreamEvent::TextDelta(chunk) = event.map_err(|e| TextCompleterError(e.to_string()))? {
                text.push_str(&chunk);
            }
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_llm::backend::LlmError;
    use aivyx_types::ContentBlock;
    use futures::stream;
    use std::sync::Mutex;

    /// A fake `LlmBackend` streaming a canned sequence of text chunks and
    /// recording the prompt it was sent — mirrors `aivyx-pa`'s own
    /// `LlmTextCompleter` test double, adapted to this trait's
    /// `BoxStream`-returning shape instead of a `next_event`/`finish`
    /// pair. `captured_prompt` uses `std::sync::Mutex`, not `tokio::sync`
    /// — the lock is held only long enough for a synchronous write/read,
    /// never across an `.await`, so the sync primitive is correct and
    /// simpler.
    struct FakeBackend {
        chunks: Vec<String>,
        captured_prompt: Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl LlmBackend for FakeBackend {
        fn model_id(&self) -> &str {
            "fake-model"
        }

        async fn stream_chat(
            &self,
            request: ChatRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError>
        {
            let prompt = request
                .messages
                .first()
                .and_then(|m| m.content.first())
                .and_then(|block| match block {
                    ContentBlock::Text(s) => Some(s.clone()),
                    _ => None,
                });
            *self.captured_prompt.lock().unwrap() = prompt;
            let events: Vec<Result<StreamEvent, LlmError>> = self
                .chunks
                .iter()
                .cloned()
                .map(|c| Ok(StreamEvent::TextDelta(c)))
                .collect();
            Ok(stream::iter(events).boxed())
        }
    }

    #[tokio::test]
    async fn complete_concatenates_multiple_streamed_chunks_in_order() {
        let backend = Arc::new(FakeBackend {
            chunks: vec![
                "<svg xmlns=\"".to_string(),
                "http://www.w3.org/2000/svg\">".to_string(),
                "<circle r=\"5\"/>".to_string(),
                "</svg>".to_string(),
            ],
            captured_prompt: Mutex::new(None),
        });
        let completer = CoderTextCompleter::new(Arc::clone(&backend) as Arc<dyn LlmBackend>, 2048);
        let result = completer.complete("draw a circle").await.unwrap();
        assert_eq!(
            result,
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"5\"/></svg>"
        );
    }

    #[tokio::test]
    async fn complete_sends_the_user_prompt_to_the_backend() {
        let backend = Arc::new(FakeBackend {
            chunks: vec!["<svg></svg>".to_string()],
            captured_prompt: Mutex::new(None),
        });
        let completer = CoderTextCompleter::new(Arc::clone(&backend) as Arc<dyn LlmBackend>, 2048);
        completer.complete("a purple hexagon icon").await.unwrap();
        assert_eq!(
            backend.captured_prompt.lock().unwrap().as_deref(),
            Some("a purple hexagon icon")
        );
    }
}
```

(This step's test module is written against this plan's own research
into `LlmBackend`'s real shape — if Step 1 found any drift, e.g. a
different `BoxStream` type path, a different `LlmError` shape, or a
different `ContentBlock`/`Message` field layout, adjust the fake to
match reality before proceeding; do not force this exact sketch to
compile against a different real API. `Arc::clone(&backend) as Arc<dyn
LlmBackend>` is used at each construction site specifically so the test
can still read `backend.captured_prompt` afterward through the original,
concretely-typed `Arc<FakeBackend>` — passing `backend` directly into
`CoderTextCompleter::new` would move it and make that read impossible.)

Register the new module in `crates/aivyx-tools/src/tools/mod.rs` (find
the existing `pub mod web_search;`-style line and add
`pub mod generate_svg_completer;` alongside it).

- [ ] **Step 6: Run to verify it fails, then implement, then verify it passes**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-tools complete_concatenates -- --nocapture
```

Expected: fails first (missing `futures`/`async-trait` as direct
dependencies of `aivyx-tools`, or a compile error from any API drift
found in Step 1 — add `futures = { workspace = true }` /
`async-trait = "0.1.89"` to `crates/aivyx-tools/Cargo.toml` if not
already present; `async-trait` already is, per this plan's own research,
but confirm), then passes once the adapter compiles against the real
API.

- [ ] **Step 7: Run full crate check**

```bash
cargo test -p aivyx-tools
cargo clippy -p aivyx-tools --all-targets -- -D warnings
```

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: add CoderTextCompleter adapter for Aivyx-Vision's TextCompleter

Wraps the agent's own already-configured Arc<dyn LlmBackend> -- no
separate, duplicate LLM connection, unlike aivyx-pa's tool-process-based
adoption (which can't share the daemon's in-memory provider across a
subprocess boundary; aivyx-coder's tools run in the same process, so
there is no such boundary here)."
```

---

## Task 2: `GenerateSvgTool`

**Files:**
- Create: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-tools/src/tools/generate_svg.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs` (add the new module,
  re-export `GenerateSvgTool` the same way `WebSearchTool` is re-exported
  — check the existing `pub use` line shape and match it)

**Interfaces:**
- Consumes: `crate::{Tool, ToolError, ToolExecutionContext}`,
  `aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget}`,
  `aivyx_types::{ToolDefinition, ToolOutput}`, `CoderTextCompleter`
  (Task 1), `aivyx_vision_svg::generate_svg`.
- Produces: `GenerateSvgTool::new(completer: Arc<dyn TextCompleter>) ->
  Self` implementing `Tool` as `"generate_svg"`.

**Grounding for the code below:**
`crates/aivyx-tools/src/tools/web_search.rs` (read it in full — this
plan's own research already read it and the shape below matches it
closely) is the template: same `needs_checkpoint() -> false` override and
rationale, same `ActionKind::Network` classification, same
`PermissionTarget::Other(...)` shape for a non-path/non-command target,
same test structure (`ctx()` helper, `mutates_outside_session`/
`needs_checkpoint` assertion pair).

- [ ] **Step 1: Write the failing tests**

```rust
use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use aivyx_vision_svg::{TextCompleter, TextCompleterError};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct GenerateSvgArgs {
    /// Description of the SVG image to generate, e.g. "a small red circle icon".
    prompt: String,
}

/// Generates a sanitized SVG image from a text prompt via the standalone
/// `aivyx-vision-svg` crate, using the agent's own already-configured
/// LLM backend (see `generate_svg_completer::CoderTextCompleter`). The
/// agent's third network-reaching tool alongside `web_search`/
/// `web_fetch` — see those tools' own doc comments for the shared
/// "local-only means the LLM backend, not network isolation" reasoning.
pub struct GenerateSvgTool {
    completer: Arc<dyn TextCompleter>,
}

impl GenerateSvgTool {
    pub fn new(completer: Arc<dyn TextCompleter>) -> Self {
        Self { completer }
    }
}

#[async_trait]
impl Tool for GenerateSvgTool {
    fn name(&self) -> &str {
        "generate_svg"
    }

    // No `mutates_outside_session` override: the trait's fail-closed
    // default (`true`) is correct, matching `web_search`/`web_fetch` --
    // a network call is not session-local, regardless of whether it
    // mutates anything.
    fn needs_checkpoint(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Generate a sanitized SVG image from a text prompt. Returns the SVG \
                markup as a string -- use write_file separately if you want to save it. \
                Uses the same LLM backend as this conversation."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GenerateSvgArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GenerateSvgArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Network,
            target: PermissionTarget::Other(args.prompt.clone()),
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
        let args: GenerateSvgArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        match aivyx_vision_svg::generate_svg(self.completer.as_ref(), &args.prompt).await {
            Ok(svg) => Ok(ToolOutput::Ok(svg)),
            Err(e) => Ok(ToolOutput::Error(format!("generate_svg: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeCompleter {
        response: Mutex<Option<Result<String, TextCompleterError>>>,
    }

    impl FakeCompleter {
        fn returning(response: Result<String, TextCompleterError>) -> Self {
            Self {
                response: Mutex::new(Some(response)),
            }
        }
    }

    #[async_trait]
    impl TextCompleter for FakeCompleter {
        async fn complete(&self, _prompt: &str) -> Result<String, TextCompleterError> {
            self.response
                .lock()
                .unwrap()
                .take()
                .expect("FakeCompleter.complete called more than once")
        }
    }

    fn ctx() -> ToolExecutionContext {
        // Match this crate's own real ToolExecutionContext test fixture
        // exactly -- see web_search.rs's own `ctx()` helper (already
        // read during this plan's research) for the real shape:
        // `cwd`, `confiner: Arc::new(aivyx_sandbox::NoopConfiner)`,
        // `cancellation: CancellationToken::new()`.
        ToolExecutionContext {
            cwd: std::env::temp_dir(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[test]
    fn mutates_outside_session_true_but_needs_checkpoint_false() {
        // Same split as web_search/web_fetch -- see those tests' own
        // comments.
        let tool = GenerateSvgTool::new(Arc::new(FakeCompleter::returning(Ok(String::new()))));
        assert!(
            tool.mutates_outside_session(),
            "generate_svg must stay hidden from plan mode"
        );
        assert!(
            !tool.needs_checkpoint(),
            "generate_svg must not trigger a git checkpoint"
        );
    }

    #[test]
    fn permission_request_is_network_tier() {
        let tool = GenerateSvgTool::new(Arc::new(FakeCompleter::returning(Ok(String::new()))));
        let request = tool
            .permission_request(&json!({"prompt": "a circle"}), Path::new("."))
            .unwrap();
        assert_eq!(request.action, ActionKind::Network);
    }

    #[tokio::test]
    async fn execute_returns_the_sanitized_svg_on_success() {
        let completer = FakeCompleter::returning(Ok(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"5\"/></svg>".to_string(),
        ));
        let tool = GenerateSvgTool::new(Arc::new(completer));
        let output = tool
            .execute(json!({"prompt": "a small circle"}), &ctx())
            .await
            .unwrap();
        let ToolOutput::Ok(svg) = output else {
            panic!("expected Ok output, got {output:?}")
        };
        assert!(svg.contains("<circle"));
    }

    #[tokio::test]
    async fn execute_reports_a_clear_error_when_generation_fails() {
        let completer = FakeCompleter::returning(Err(TextCompleterError("backend down".into())));
        let tool = GenerateSvgTool::new(Arc::new(completer));
        let output = tool
            .execute(json!({"prompt": "anything"}), &ctx())
            .await
            .unwrap();
        let ToolOutput::Error(msg) = output else {
            panic!("expected Error output, got {output:?}")
        };
        assert!(msg.contains("backend down"));
    }

    #[tokio::test]
    async fn execute_rejects_a_missing_prompt_field() {
        let tool = GenerateSvgTool::new(Arc::new(FakeCompleter::returning(Ok(String::new()))));
        let result = tool.execute(json!({}), &ctx()).await;
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }
}
```

(Whether `ToolOutput` derives `Debug` — needed for the `{output:?}`/
`{other:?}` panic messages above — confirm this in Step 1's grep of
`aivyx-types/src/lib.rs`; if it doesn't, either add the derive there or
match on the variant without the debug-format panic message.)

- [ ] **Step 2: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-tools generate_svg -- --nocapture
```

Expected: compile error — `GenerateSvgTool` and its module don't exist
yet (register the module + `pub use` in `mod.rs` as described above
before this will even attempt to compile the test file).

- [ ] **Step 3: Confirm they pass**

```bash
cargo test -p aivyx-tools generate_svg -- --nocapture
```

Expected: all 5 new tests pass.

- [ ] **Step 4: Run full crate check**

```bash
cargo test -p aivyx-tools
cargo clippy -p aivyx-tools --all-targets -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: add GenerateSvgTool (generate_svg)

The one tool this milestone adds to aivyx-coder. Input: {prompt: string}.
Output: sanitized SVG markup as plain text -- the model uses write_file
separately if it wants to save it. Classified ActionKind::Network
(matches web_search/web_fetch's own reasoning: a network call is not
session-local regardless of whether it mutates anything), needs_checkpoint
= false (no filesystem mutation for a checkpoint to protect)."
```

---

## Task 3: Register the tool + docs + final verification

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx/src/agent_builder.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/README.md` (tool table,
  if one exists documenting each tool — check the file's structure first)

**Interfaces:** none new — wiring + documentation only.

- [ ] **Step 1: Read the real registration point and confirm `llm`'s availability there**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
grep -n "let llm: Arc<dyn LlmBackend>\|let mut registry = ToolRegistry::new()\|registry.register(Arc::new(WebSearchTool" crates/aivyx/src/agent_builder.rs
```

Confirm `llm` (the already-built `Arc<dyn LlmBackend>`) is in scope at
the point tools are registered — this plan's own research found it built
at (approximately) line 172, well before tool registration at
(approximately) line 315+, so it should already be available; if a
refactor since then moved either, adjust accordingly.

- [ ] **Step 2: Register the tool**

Near the `registry.register(Arc::new(WebSearchTool::new(...)))` line, add:

```rust
let vision_completer: Arc<dyn aivyx_vision_svg::TextCompleter> =
    Arc::new(aivyx_tools::generate_svg_completer::CoderTextCompleter::new(
        Arc::clone(&llm),
        2048,
    ));
registry.register(Arc::new(aivyx_tools::GenerateSvgTool::new(vision_completer)));
```

(Confirm the exact re-export path for `GenerateSvgTool` and
`generate_svg_completer` from `aivyx_tools`'s crate root — match whatever
`pub use`/`pub mod` shape Task 1/Task 2 actually established, adjusting
this call site's paths if they differ from the sketch above. `2048` is
this plan's chosen `max_tokens` ceiling — smaller than `aivyx-pa`'s
`4096` since `aivyx-coder`'s target backends skew toward smaller local
models; not a hard requirement, adjust if you have a stronger reason
during implementation.)

- [ ] **Step 3: Check whether `README.md` documents individual tools, and update it if so**

```bash
grep -n "web_search\|write_file\|read_file" README.md | head -10
```

If a tool-by-tool table or list exists, add `generate_svg` following the
same format. If tools aren't documented individually there (e.g. only
described in aggregate), skip this — don't invent a new documentation
section this repo doesn't already have a place for.

- [ ] **Step 4: Full workspace verification**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace 2>&1 | tail -40
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all green — this is the first time this plan's changes are
checked against everything else in the workspace at once, not just
`aivyx-tools`.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: register generate_svg in the agent's tool registry"
```

---

## Final verification (after all 3 tasks land)

- [ ] Run the complete workspace test suite once, not per-crate:

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test --workspace 2>&1 | tail -30
```

- [ ] Run the documented clippy command once more:

```bash
cargo clippy --workspace --all-targets
```

- [ ] This plan does not decide whether to push a branch / open a PR —
  follow `superpowers:finishing-a-development-branch` once all tasks are
  individually reviewed and a final whole-branch review has passed, same
  as `aivyx-pa`'s own adoption plan.
