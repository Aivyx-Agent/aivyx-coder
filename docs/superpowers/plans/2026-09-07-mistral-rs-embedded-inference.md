# Embedded mistral.rs Inference Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `aivyx-coder` an embedded, in-process GGUF inference backend (`mistral.rs`), matching the capability `aivyx` (the sibling flagship) already shipped in its own Phase 134 — with real token streaming from day one, not the non-streaming compromise `aivyx`'s own version shipped and never followed up on.

**Architecture:** A new `MistralRsBackend` implementing the existing `LlmBackend` trait unchanged, in a new `crates/aivyx-llm/src/mistral_rs/` module (mirroring `aivyx`'s own module layout: `mod.rs`, `provider.rs`, `convert.rs`). The real streaming fix: mistral.rs's `Model::stream_chat_request` returns a `Stream<'_>` borrowed from the model, which can't satisfy the trait's `BoxStream<'static, ...>` requirement directly — so the borrowed stream is driven entirely inside a `tokio::spawn`ed task (which owns its own `Arc<Model>` clone for its whole life, satisfying the borrow), forwarding converted events out through an `mpsc` channel wrapped as `ReceiverStream` — which is genuinely `'static` since it borrows nothing.

**Tech Stack:** Rust, `mistralrs = "=0.8.*"` (new optional dependency), `tokio::sync::mpsc` + `tokio_stream::wrappers::ReceiverStream` (the streaming shim), `async_trait`.

## Global Constraints

- Feature flag names must exactly match `aivyx`'s own: `provider-mistral-rs`, `provider-mistral-rs-cuda`, `provider-mistral-rs-metal`, `provider-mistral-rs-accelerate`.
- `mistralrs` pinned to exactly `"=0.8.*"`, same as `aivyx`'s own pin.
- New config fields are flat fields directly on `BackendSettings` (this repo's own convention) — never a new `[mistralrs]` TOML table.
- The streaming shim must produce a genuinely `'static` `BoxStream` via the mpsc/`ReceiverStream` pattern — no `ouroboros` or other self-referential-struct dependency.
- No changes to the `LlmBackend` trait itself, and no changes to `OpenAiCompatBackend` — this is a purely additive new implementation plus one new dispatch arm.
- **This environment has no real GGUF model file and no GPU.** Every test in this plan must run without either — anything that would otherwise need a real loaded `mistralrs::Model` is tested through a generic, fake-stream-driven helper instead (see Task 4).

---

### Task 1: Dependency and feature flags

**Files:**
- Modify: `crates/aivyx-llm/Cargo.toml`

**Interfaces:**
- Consumes: nothing (first task).
- Produces: the `provider-mistral-rs`/`-cuda`/`-metal`/`-accelerate` Cargo features, available to every later task in this plan.

- [ ] **Step 1: Add the dependency and feature flags**

In `crates/aivyx-llm/Cargo.toml`, add to `[dependencies]`:

```toml
mistralrs = { version = "=0.8.*", default-features = false, optional = true }
tokio-stream = "0.1"
```

(`tokio-stream` is the standard, minimal crate providing `ReceiverStream` — check first whether it's already a dependency anywhere in this workspace via `grep -rn "tokio-stream" Cargo.lock`; if it's already present at a compatible version, reuse that version instead of introducing a second one.)

Add a `[features]` section (create it if it doesn't exist — confirmed this session that `aivyx-llm/Cargo.toml` has none today):

```toml
[features]
provider-mistral-rs = ["dep:mistralrs"]
provider-mistral-rs-cuda = ["provider-mistral-rs", "mistralrs?/cuda"]
provider-mistral-rs-metal = ["provider-mistral-rs", "mistralrs?/metal"]
provider-mistral-rs-accelerate = ["provider-mistral-rs", "mistralrs?/accelerate"]
```

- [ ] **Step 2: Verify the dependency resolves and starts compiling**

Run: `cd crates/aivyx-llm && cargo build --features provider-mistral-rs`
Expected: mistralrs and its transitive dependencies (`candle`, `hf-hub`, `tokenizers`, etc.) download and at least start compiling. This will take several minutes on a cold cache — that's expected per the design spec's own honest scope note. If it fails with a version-resolution error or an unknown-feature error, fix the `Cargo.toml` entry before proceeding — catching a bad pin or feature-name typo here, before any code depends on it, is exactly the point of this task existing on its own.

- [ ] **Step 3: Confirm the default (no-feature) build is unaffected**

Run: `cd crates/aivyx-llm && cargo build`
Expected: builds exactly as before, mistralrs not compiled at all (optional + not enabled by default).

- [ ] **Step 4: Locate mistral.rs's real streaming API in the downloaded source**

The exact method name and return type were confirmed via `docs.rs` for a nearby version but must be re-confirmed against the exact `=0.8.*` version actually resolved, since this crate is pre-1.0 and its own maintainers warn its API churns between minor releases. Run:

```bash
find ~/.cargo/registry/src -maxdepth 1 -iname "mistralrs-core-0.8*" -o -iname "mistralrs-0.8*" 2>/dev/null
```

Then `grep -n "pub async fn stream_chat_request\|pub async fn send_chat_request\|pub enum Response" <that path>/src/*.rs` (or wherever the `Model` struct and `Response` enum are actually defined in the resolved version) to confirm:
- `stream_chat_request`'s exact signature (expected shape, confirmed against a nearby version: `pub async fn stream_chat_request<R: RequestLike>(&self, request: R) -> Result<Stream<'_>>`, yielding `Response` items).
- `Response`'s exact variants (expected: `Chunk(ChatCompletionChunkResponse)` for streamed deltas, `Done(ChatCompletionResponse)` for the terminal signal, plus error variants — confirm the exact names and the exact fields on `ChatCompletionChunkResponse` needed to extract delta text, since Task 4 needs these precisely).

Record what you find in your task report — Task 4 depends on this being accurate, not approximated.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-llm/Cargo.toml Cargo.lock
git commit -m "chore: add optional mistralrs dependency and provider-mistral-rs feature flags

Mirrors aivyx's own Phase 134 feature naming exactly (provider-mistral-rs,
-cuda, -metal, -accelerate) for cross-repo consistency. No code uses it
yet -- this task only proves the dependency resolves and the default
build is unaffected."
```

---

### Task 2: `BackendKind::MistralRs` + config fields

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs`

**Interfaces:**
- Consumes: nothing from Task 1 directly (pure config, no dependency on the `mistralrs` crate itself).
- Produces: `BackendKind::MistralRs` variant; `BackendSettings`'s 5 new fields (`mistralrs_model_path: Option<String>`, `mistralrs_model_file: Option<String>`, `mistralrs_chat_template_path: Option<String>`, `mistralrs_max_seq_len: Option<usize>`, `mistralrs_constrain_tool_calls: bool`) — consumed by Task 5's dispatch.

- [ ] **Step 1: Write the failing tests**

Find the `#[cfg(test)] mod tests` block in `crates/aivyx-config/src/lib.rs` (search for `fn default_deny_paths_covers_common_credential_locations` or any nearby existing `BackendSettings`/`BackendKind` test to find the right module). Add:

```rust
    #[test]
    fn backend_kind_parses_mistral_rs() {
        let toml_str = r#"
            [backend]
            kind = "mistral_rs"
        "#;
        let settings: Settings = toml::from_str(toml_str).expect("parse");
        assert_eq!(settings.backend.kind, BackendKind::MistralRs);
    }

    #[test]
    fn backend_settings_default_has_no_mistralrs_fields_set() {
        let settings = Settings::default();
        assert!(settings.backend.mistralrs_model_path.is_none());
        assert!(settings.backend.mistralrs_model_file.is_none());
        assert!(settings.backend.mistralrs_chat_template_path.is_none());
        assert!(settings.backend.mistralrs_max_seq_len.is_none());
        assert!(!settings.backend.mistralrs_constrain_tool_calls);
    }

    #[test]
    fn backend_settings_mistralrs_fields_round_trip_through_toml() {
        let toml_str = r#"
            [backend]
            kind = "mistral_rs"
            mistralrs_model_path = "/models/qwen3-4b.gguf"
            mistralrs_model_file = "qwen3-4b-q4_k_m.gguf"
            mistralrs_chat_template_path = "/templates/qwen3.json"
            mistralrs_max_seq_len = 8192
            mistralrs_constrain_tool_calls = true
        "#;
        let settings: Settings = toml::from_str(toml_str).expect("parse");
        assert_eq!(
            settings.backend.mistralrs_model_path.as_deref(),
            Some("/models/qwen3-4b.gguf")
        );
        assert_eq!(
            settings.backend.mistralrs_model_file.as_deref(),
            Some("qwen3-4b-q4_k_m.gguf")
        );
        assert_eq!(
            settings.backend.mistralrs_chat_template_path.as_deref(),
            Some("/templates/qwen3.json")
        );
        assert_eq!(settings.backend.mistralrs_max_seq_len, Some(8192));
        assert!(settings.backend.mistralrs_constrain_tool_calls);
    }

    #[test]
    fn backend_kind_mistral_rs_parsing_does_not_require_the_new_fields() {
        // Parsing itself must never fail just because the model path is
        // absent -- that's a dispatch-time (Task 5) validation error, not
        // a config-load-time one, matching how llama-server's own
        // reachability isn't checked at load time either.
        let toml_str = r#"
            [backend]
            kind = "mistral_rs"
        "#;
        let settings: Settings = toml::from_str(toml_str).expect("parse must succeed");
        assert_eq!(settings.backend.kind, BackendKind::MistralRs);
        assert!(settings.backend.mistralrs_model_path.is_none());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-config mistral`
Expected: FAIL to compile — `no variant MistralRs on BackendKind`, `no field mistralrs_model_path on BackendSettings`.

- [ ] **Step 3: Add the `MistralRs` variant and the 5 new fields**

Find `pub enum BackendKind` (currently):

```rust
pub enum BackendKind {
    #[default]
    Generic,
    LlamaServer,
}
```

Replace with:

```rust
pub enum BackendKind {
    #[default]
    Generic,
    LlamaServer,
    /// A genuinely different backend implementation
    /// (`MistralRsBackend`, `crates/aivyx-llm/src/mistral_rs/`) is
    /// constructed for this variant -- unlike `LlamaServer`, which only
    /// toggles extra features on the same `OpenAiCompatBackend`. See
    /// `agent_builder.rs`'s dispatch on this enum.
    MistralRs,
}
```

Find `pub struct BackendSettings`'s last field (currently ending with `pub kvcache_store_path: Option<String>,` followed by the closing `}`). Add the 5 new fields right before the closing `}`:

```rust
    pub kvcache_store_path: Option<String>,
    /// Absolute path to a local GGUF file or a directory containing GGUF
    /// files. Required when `kind = "mistral_rs"` -- checked at
    /// backend-construction time (agent_builder.rs), not at config-load
    /// time, matching how llama-server's own reachability isn't checked
    /// at load time either.
    pub mistralrs_model_path: Option<String>,
    /// Selects a specific `.gguf` file inside `mistralrs_model_path`
    /// when it's a directory containing more than one candidate.
    pub mistralrs_model_file: Option<String>,
    /// Overrides the chat template mistral.rs would otherwise infer
    /// from the model's own metadata.
    pub mistralrs_chat_template_path: Option<String>,
    /// Maximum sequence length mistral.rs allocates KV-cache space for.
    /// `None` lets mistral.rs pick its own default.
    pub mistralrs_max_seq_len: Option<usize>,
    /// Grammar-constrained tool-calling via mistral.rs's own JSON-Schema
    /// constraint support, mirroring aivyx's own Chapter Stencil
    /// equivalent. Default off.
    #[serde(default)]
    pub mistralrs_constrain_tool_calls: bool,
}
```

- [ ] **Step 4: Update `BackendSettings::default()`**

Find `impl Default for BackendSettings`'s body (currently ending `kvcache_store_path: None,`). Add the 5 new defaults:

```rust
            kvcache_store_path: None,
            mistralrs_model_path: None,
            mistralrs_model_file: None,
            mistralrs_chat_template_path: None,
            mistralrs_max_seq_len: None,
            mistralrs_constrain_tool_calls: false,
        }
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p aivyx-config mistral`
Expected: 4 passed.

- [ ] **Step 6: Run the full config crate suite**

Run: `cargo test -p aivyx-config`
Expected: all tests pass (same count as before plus the 4 new ones).

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-config/src/lib.rs
git commit -m "feat: add BackendKind::MistralRs and its 5 config fields

Flat fields directly on BackendSettings, matching this repo's own
convention (same as kvcache_store_path/kvcache_max_bytes) rather than a
new [mistralrs] TOML table. Fields all optional/default-absent -- config
parsing never fails just because they're unset; the required-when-selected
check happens at backend-construction time (Task 5)."
```

---

### Task 3: The conversion layer (`convert.rs`)

**Files:**
- Create: `crates/aivyx-llm/src/mistral_rs/convert.rs`

**Interfaces:**
- Consumes: `aivyx_types::{Message, Role, ContentBlock, ToolCall, ToolCallId, ToolResult, ToolOutput, ToolDefinition}` (all already defined in `crates/aivyx-types/src/lib.rs`, unchanged by this plan); `aivyx-llm`'s own `ChatRequest`/`ToolChoice` (`crates/aivyx-llm/src/backend.rs`, unchanged).
- Produces: `pub fn append_message_to_builder(builder: mistralrs::RequestBuilder, message: &aivyx_types::Message) -> mistralrs::RequestBuilder`, `pub fn to_mistralrs_tool(desc: &aivyx_types::ToolDefinition) -> Result<mistralrs::Tool, String>`, `pub fn apply_tools(builder: mistralrs::RequestBuilder, tools: &[aivyx_types::ToolDefinition]) -> Result<mistralrs::RequestBuilder, String>` — all consumed by Task 4's `MistralRsBackend`.

This entire file is gated `#[cfg(feature = "provider-mistral-rs")]` at the module-declaration site (Task 4's `mod.rs`), not per-function here.

**Important structural note, grounded this session:** `aivyx-coder`'s `Message` is a single struct (`{ role: Role, content: Vec<ContentBlock>, tool_call_id: Option<ToolCallId> }`), unlike `aivyx`'s own per-role-variant `LlmMessage` enum. A tool call or tool result can appear as a `ContentBlock` *inside* an `Assistant`/`Tool`-role message's `content` vec (`ContentBlock::{Text(String), ToolCall(ToolCall), ToolResult(ToolResult)}`), rather than as a separate top-level field. The conversion function below handles this directly.

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-llm/src/mistral_rs/convert.rs` with just the test module first (TDD — the functions don't exist yet):

```rust
//! Pure conversion between aivyx-coder's request/response types and
//! mistral.rs's. No async, no IO -- directly unit-testable without a
//! model load, mirroring aivyx's own convert.rs test pattern (construct
//! a bare `RequestBuilder::new()` and call the conversion functions
//! against it directly).

use aivyx_types::{ContentBlock, Message, Role, ToolCall, ToolCallId, ToolCallSource, ToolDefinition, ToolOutput, ToolResult};
use mistralrs::{RequestBuilder, TextMessageRole, ToolChoice as MistralRsToolChoice};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn append_user_text_message() {
        let builder = RequestBuilder::new();
        let msg = Message {
            role: Role::User,
            content: vec![ContentBlock::Text("ping".to_string())],
            tool_call_id: None,
        };
        let _builder = append_message_to_builder(builder, &msg);
        // No public accessor on RequestBuilder to inspect state directly
        // (same caveat aivyx's own convert.rs tests document) -- this
        // proves the conversion compiles and runs on the happy path.
    }

    #[test]
    fn append_assistant_message_with_inline_tool_call_content_block() {
        let builder = RequestBuilder::new();
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text("Let me check that.".to_string()),
                ContentBlock::ToolCall(ToolCall {
                    id: ToolCallId("call_abc".to_string()),
                    name: "fs.read".to_string(),
                    arguments: json!({"path": "/etc/hosts"}),
                    source: ToolCallSource::Native,
                }),
            ],
            tool_call_id: None,
        };
        let _builder = append_message_to_builder(builder, &msg);
    }

    #[test]
    fn append_tool_result_message() {
        let builder = RequestBuilder::new();
        let msg = Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult(ToolResult {
                call_id: ToolCallId("call_abc".to_string()),
                output: ToolOutput::Ok("127.0.0.1 localhost".to_string()),
            })],
            tool_call_id: Some(ToolCallId("call_abc".to_string())),
        };
        let _builder = append_message_to_builder(builder, &msg);
    }

    #[test]
    fn append_tool_result_message_with_denied_output_still_converts() {
        let builder = RequestBuilder::new();
        let msg = Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult(ToolResult {
                call_id: ToolCallId("call_xyz".to_string()),
                output: ToolOutput::Denied("operator declined".to_string()),
            })],
            tool_call_id: Some(ToolCallId("call_xyz".to_string())),
        };
        let _builder = append_message_to_builder(builder, &msg);
    }

    #[test]
    fn to_mistralrs_tool_forwards_schema_verbatim() {
        let desc = ToolDefinition {
            name: "fs.read".to_string(),
            description: "Read a file from disk".to_string(),
            parameters_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            }),
        };
        let tool = to_mistralrs_tool(&desc).expect("ok");
        assert_eq!(tool.function.name, "fs.read");
        assert_eq!(tool.function.description.as_deref(), Some("Read a file from disk"));
        let params = tool.function.parameters.as_ref().expect("present");
        assert!(params.contains_key("type"));
        assert!(params.contains_key("properties"));
        assert!(params.contains_key("required"));
    }

    #[test]
    fn to_mistralrs_tool_rejects_non_object_schema() {
        let desc = ToolDefinition {
            name: "weird".to_string(),
            description: "schema isn't an object".to_string(),
            parameters_schema: json!("nope"),
        };
        let e = to_mistralrs_tool(&desc).expect_err("must error");
        assert!(e.contains("weird"), "{e}");
        assert!(e.contains("object"), "{e}");
    }

    #[test]
    fn apply_tools_empty_is_noop() {
        let builder = RequestBuilder::new();
        apply_tools(builder, &[]).expect("ok");
    }

    #[test]
    fn apply_tools_with_one_tool_succeeds() {
        let builder = RequestBuilder::new();
        let tools = vec![ToolDefinition {
            name: "fs.read".to_string(),
            description: "read".to_string(),
            parameters_schema: json!({"type": "object", "properties": {}}),
        }];
        apply_tools(builder, &tools).expect("ok");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-llm --features provider-mistral-rs mistral_rs::convert`
Expected: FAIL to compile — the functions don't exist yet, and `mistral_rs` isn't declared as a module yet either (that's Task 4's `mod.rs` — for this task, temporarily add `#[cfg(feature = "provider-mistral-rs")] mod mistral_rs;` with `pub mod convert;` inside it to `crates/aivyx-llm/src/lib.rs` so this file compiles in isolation; Task 4 will flesh out the rest of `mod.rs`).

- [ ] **Step 3: Implement the conversion functions**

Add above the test module in the same file:

```rust
/// Add a single aivyx-coder `Message` to a mistral.rs `RequestBuilder`.
/// A message's `content` vec may contain plain text, an inline tool
/// call (on an `Assistant`-role message), or an inline tool result (on
/// a `Tool`-role message) -- aivyx-coder unifies these as `ContentBlock`
/// variants rather than separate per-role fields the way aivyx's own
/// `LlmMessage` enum does.
pub fn append_message_to_builder(mut builder: RequestBuilder, message: &Message) -> RequestBuilder {
    match message.role {
        Role::System => {
            let text = flatten_text_blocks(&message.content);
            builder.add_message(TextMessageRole::System, text)
        }
        Role::User => {
            let text = flatten_text_blocks(&message.content);
            builder.add_message(TextMessageRole::User, text)
        }
        Role::Assistant => {
            let text = flatten_text_blocks(&message.content);
            let tool_calls: Vec<_> = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolCall(tc) => Some(tc),
                    _ => None,
                })
                .collect();
            if tool_calls.is_empty() {
                builder.add_message(TextMessageRole::Assistant, text)
            } else {
                let mistralrs_calls: Vec<_> = tool_calls
                    .iter()
                    .enumerate()
                    .map(|(idx, tc)| mistralrs::ToolCallResponse {
                        index: idx,
                        id: tc.id.0.clone(),
                        tp: mistralrs::ToolCallType::Function,
                        function: mistralrs::CalledFunction {
                            name: tc.name.clone(),
                            arguments: tc.arguments.to_string(),
                        },
                    })
                    .collect();
                builder.add_message_with_tool_call(TextMessageRole::Assistant, text, mistralrs_calls)
            }
        }
        Role::Tool => {
            // A Tool-role message's content holds exactly one
            // ContentBlock::ToolResult in every real call site today
            // (aivyx-coder's own agent loop constructs it that way) --
            // take the first one found, defensively ignoring anything
            // else rather than panicking on an unexpected shape.
            let result_text = message
                .content
                .iter()
                .find_map(|block| match block {
                    ContentBlock::ToolResult(tr) => Some(tool_output_to_text(&tr.output)),
                    _ => None,
                })
                .unwrap_or_default();
            let call_id = message
                .tool_call_id
                .as_ref()
                .map(|id| id.0.clone())
                .unwrap_or_default();
            builder = builder.add_tool_message(result_text, call_id);
            builder
        }
    }
}

fn flatten_text_blocks(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_output_to_text(output: &ToolOutput) -> String {
    match output {
        ToolOutput::Ok(s) => s.clone(),
        ToolOutput::Error(s) => format!("Error: {s}"),
        ToolOutput::Denied(s) => format!("Denied: {s}"),
    }
}

/// Map an aivyx-coder `ToolDefinition` to a mistral.rs `Tool`.
/// mistral.rs's `Function::parameters` is `HashMap<String, Value>` (the
/// top-level keys of the JSON-Schema object), not the wrapped object
/// aivyx-coder's own `parameters_schema` carries -- extract the keys.
pub fn to_mistralrs_tool(desc: &ToolDefinition) -> Result<mistralrs::Tool, String> {
    let parameters = match &desc.parameters_schema {
        serde_json::Value::Object(map) => {
            let hm: std::collections::HashMap<String, serde_json::Value> =
                map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            Some(hm)
        }
        _ => {
            return Err(format!(
                "tool `{}` has a non-object parameters_schema; mistralrs requires object",
                desc.name
            ));
        }
    };
    Ok(mistralrs::Tool {
        tp: mistralrs::ToolType::Function,
        function: mistralrs::Function {
            description: Some(desc.description.clone()),
            name: desc.name.clone(),
            parameters,
        },
    })
}

/// Apply aivyx-coder's tool catalog onto a mistral.rs builder. Empty
/// tool list leaves the builder unchanged.
pub fn apply_tools(
    builder: RequestBuilder,
    tools: &[ToolDefinition],
) -> Result<RequestBuilder, String> {
    if tools.is_empty() {
        return Ok(builder);
    }
    let converted: Result<Vec<_>, _> = tools.iter().map(to_mistralrs_tool).collect();
    Ok(builder.set_tools(converted?).set_tool_choice(MistralRsToolChoice::Auto))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-llm --features provider-mistral-rs mistral_rs::convert`
Expected: 8 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-llm/src/mistral_rs/convert.rs crates/aivyx-llm/src/lib.rs
git commit -m "feat: add mistral.rs conversion layer (Message/ToolDefinition -> mistralrs types)

Pure functions, no async/IO -- directly unit-testable without a model
load, mirroring aivyx's own convert.rs test pattern. Handles
aivyx-coder's own Message shape, where tool calls/results are inline
ContentBlock variants rather than separate per-role fields."
```

---

### Task 4: `MistralRsBackend` + the streaming shim

**Files:**
- Create: `crates/aivyx-llm/src/mistral_rs/provider.rs`
- Create: `crates/aivyx-llm/src/mistral_rs/mod.rs`
- Modify: `crates/aivyx-llm/src/lib.rs` (module declaration)

**Interfaces:**
- Consumes: `append_message_to_builder`, `apply_tools` from Task 3's `convert.rs`; `BackendSettings`'s 5 fields from Task 2 (via the struct passed into the constructor, not read directly — see below); `LlmBackend`, `ChatRequest`, `StreamEvent`, `LlmError` from `crates/aivyx-llm/src/backend.rs` (unchanged).
- Produces: `pub struct MistralRsBackend` with `pub async fn new(model_path: PathBuf, model_file: Option<String>, chat_template_path: Option<PathBuf>, max_seq_len: Option<usize>, constrain_tool_calls: bool) -> Result<Self, LlmError>`, implementing `LlmBackend` — consumed by Task 5's dispatch. Also produces the generic, independently-testable `forward_stream_via_mpsc` helper (private to this module — Task 5 doesn't need it directly).

- [ ] **Step 1: Confirm the exact mistral.rs response types from Task 1's grounding**

Before writing any code, re-read what Task 1's Step 4 recorded about `Response`'s exact variants and `ChatCompletionChunkResponse`'s exact fields (from the real downloaded `=0.8.*` source, not the nearby-version docs.rs page this plan was written against). If Task 1's report is missing this detail, re-run Task 1 Step 4's grep yourself before proceeding — the streaming shim's conversion function needs the real field names, not approximated ones.

- [ ] **Step 2: Write the failing tests for the generic forwarding helper**

Create `crates/aivyx-llm/src/mistral_rs/provider.rs`:

```rust
//! `MistralRsBackend` -- aivyx-coder's `LlmBackend` impl over an
//! in-process `mistralrs` model, with real token streaming (unlike
//! aivyx's own Phase 134 MistralRsProvider, which shipped
//! non-streaming and never followed up).
//!
//! ## The streaming shim
//!
//! mistral.rs's `Model::stream_chat_request` returns a `Stream<'_>`
//! borrowed from the model, which can't satisfy this trait's
//! `BoxStream<'static, ...>` requirement directly. The fix: drive the
//! borrowed stream entirely inside a spawned task holding its own
//! `Arc<Model>` clone (the borrow is satisfied within that task's own
//! scope), forwarding converted, fully-owned events out through an
//! `mpsc` channel wrapped as `ReceiverStream` -- which is genuinely
//! `'static` since it borrows nothing.
//!
//! `forward_stream_via_mpsc` is deliberately generic over the source
//! stream's item type, not tied to mistral.rs's own `Stream<'_>` --
//! this is what makes the forwarding *logic itself* (ordering,
//! stop-on-receiver-drop, error propagation) testable with a fake
//! stream, in an environment with no real GGUF model or GPU.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{Stream, StreamExt};
use mistralrs::{GgufModelBuilder, Model};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatRequest, LlmBackend, LlmError, StreamEvent};
use crate::mistral_rs::convert::{append_message_to_builder, apply_tools};

/// Drives `stream` to completion, forwarding each item (after
/// conversion via `convert`) through `tx`. Stops early if the receiver
/// has been dropped (`tx.send` fails) -- the caller lost interest, so
/// continuing to drive inference would waste CPU/GPU on a turn nobody
/// is reading anymore. Generic over the source item type `T` so this
/// can be tested with a fake stream instead of a real mistral.rs one.
async fn forward_stream_via_mpsc<S, T>(
    mut stream: S,
    tx: mpsc::Sender<Result<StreamEvent, LlmError>>,
    convert: impl Fn(T) -> Result<StreamEvent, LlmError>,
) where
    S: Stream<Item = T> + Unpin,
{
    while let Some(item) = stream.next().await {
        let event = convert(item);
        if tx.send(event).await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod forwarding_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn forwards_items_in_order() {
        let (tx, rx) = mpsc::channel(8);
        let source = futures::stream::iter(vec![1, 2, 3]);
        forward_stream_via_mpsc(source, tx, |n: i32| {
            Ok(StreamEvent::TextDelta(n.to_string()))
        })
        .await;

        let received: Vec<_> = ReceiverStream::new(rx).collect().await;
        assert_eq!(received.len(), 3);
        for (i, event) in received.into_iter().enumerate() {
            match event.expect("ok") {
                StreamEvent::TextDelta(s) => assert_eq!(s, (i as i32 + 1).to_string()),
                other => panic!("expected TextDelta, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn forwards_a_conversion_error_rather_than_panicking_or_dropping_it() {
        let (tx, rx) = mpsc::channel(8);
        let source = futures::stream::iter(vec![1, 2]);
        forward_stream_via_mpsc(source, tx, |n: i32| {
            if n == 2 {
                Err(LlmError::Parse("boom".to_string()))
            } else {
                Ok(StreamEvent::TextDelta(n.to_string()))
            }
        })
        .await;

        let received: Vec<_> = ReceiverStream::new(rx).collect().await;
        assert_eq!(received.len(), 2);
        assert!(received[0].is_ok());
        assert!(matches!(received[1], Err(LlmError::Parse(_))));
    }

    #[tokio::test]
    async fn stops_driving_the_source_once_the_receiver_is_dropped() {
        // A stream that counts how many times it's been polled, and
        // never ends -- if `forward_stream_via_mpsc` doesn't stop on
        // receiver-drop, this test would hang forever.
        struct CountingInfiniteStream {
            polls: Arc<AtomicUsize>,
        }
        impl Stream for CountingInfiniteStream {
            type Item = i32;
            fn poll_next(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<i32>> {
                self.polls.fetch_add(1, Ordering::SeqCst);
                std::task::Poll::Ready(Some(0))
            }
        }

        let polls = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel(1);
        let source = CountingInfiniteStream { polls: Arc::clone(&polls) };

        // Drop the receiver immediately -- the very next `tx.send` must fail.
        drop(rx);

        forward_stream_via_mpsc(source, tx, |n: i32| Ok(StreamEvent::TextDelta(n.to_string())))
            .await;

        // Must have stopped after (at most) the first failed send -- not
        // spun forever driving an infinite source with nowhere to send.
        assert!(
            polls.load(Ordering::SeqCst) <= 1,
            "expected the forwarding loop to stop almost immediately after the \
             receiver was dropped, but the source was polled {} times",
            polls.load(Ordering::SeqCst)
        );
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p aivyx-llm --features provider-mistral-rs forwarding_tests`
Expected: FAIL to compile (`forward_stream_via_mpsc` exists in this same file already, written in Step 2 above alongside its tests, so this should actually compile and pass at this point — if you followed TDD strictly by writing the tests *before* `forward_stream_via_mpsc`, split Step 2 into "tests only" then re-add the function in a real Step 3; either ordering is fine as long as you run the tests once before the implementation exists and confirm a real failure, per this plan's own TDD discipline).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-llm --features provider-mistral-rs forwarding_tests`
Expected: 3 passed.

- [ ] **Step 5: Implement `MistralRsBackend` itself**

Add to the same `provider.rs` file (above the test module):

```rust
/// In-process LLM backend over a loaded mistral.rs model. The model is
/// loaded once at construction and reused across every `stream_chat`
/// call via a cheap `Arc` clone.
pub struct MistralRsBackend {
    model: Arc<Model>,
    constrain_tool_calls: bool,
}

impl MistralRsBackend {
    /// Loads the configured GGUF model. Async because mistral.rs's
    /// builder performs the load (mmap + tokenizer init + chat template
    /// parse) itself.
    pub async fn new(
        model_path: PathBuf,
        model_file: Option<String>,
        chat_template_path: Option<PathBuf>,
        _max_seq_len: Option<usize>,
        constrain_tool_calls: bool,
    ) -> Result<Self, LlmError> {
        let (dir, files) = match &model_file {
            Some(f) => (model_path.to_string_lossy().to_string(), vec![f.clone()]),
            None => {
                let parent = model_path
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| ".".to_string());
                let filename = model_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .ok_or_else(|| {
                        LlmError::Config(format!(
                            "mistralrs: cannot extract filename from {model_path:?}"
                        ))
                    })?;
                (parent, vec![filename])
            }
        };

        let mut builder = GgufModelBuilder::new(dir, files);
        if let Some(template) = &chat_template_path {
            builder = builder.with_chat_template(template.to_string_lossy().to_string());
        }
        let model = builder
            .build()
            .await
            .map_err(|e| LlmError::Config(format!("mistralrs: model load failed: {e}")))?;

        Ok(MistralRsBackend {
            model: Arc::new(model),
            constrain_tool_calls,
        })
    }
}

/// Converts one mistral.rs `Response` item into aivyx-coder's own
/// `StreamEvent`. Ground the exact `Response`/`ChatCompletionChunkResponse`
/// field names against the real downloaded `=0.8.*` source (Task 1 Step
/// 4 / this task's own Step 1) before finalizing this match -- the
/// variant names below are confirmed against a nearby version's public
/// docs, not the exact pinned version's source.
fn convert_response_to_stream_event(response: mistralrs::Response) -> Result<StreamEvent, LlmError> {
    match response {
        mistralrs::Response::Chunk(chunk) => {
            // Extract the delta text from chunk.choices[0].delta.content
            // (or the equivalent real field path confirmed in Step 1).
            // Ground this exactly against the real source before
            // shipping -- do not guess the field path.
            todo!("fill in from the real ChatCompletionChunkResponse shape confirmed in Step 1")
        }
        mistralrs::Response::Done(done) => Ok(StreamEvent::Done {
            finish_reason: crate::backend::FinishReason::Stop,
        }),
        other => Err(LlmError::Parse(format!(
            "mistralrs: unexpected response variant in chat stream: {other:?}"
        ))),
    }
}

#[async_trait]
impl LlmBackend for MistralRsBackend {
    fn model_id(&self) -> &str {
        "mistralrs-embedded"
    }

    async fn stream_chat(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        let model = Arc::clone(&self.model);
        let mut builder = mistralrs::RequestBuilder::new();
        for msg in &request.messages {
            builder = append_message_to_builder(builder, msg);
        }
        // NOTE: aivyx-coder's ChatRequest carries `tools: Vec<ToolDefinition>`
        // directly (unlike the message-conversion path); wire this
        // through `apply_tools` here once you've confirmed the exact
        // field name on `ChatRequest` (grounded in Task 3/backend.rs
        // already -- it's `tools`).
        builder = apply_tools(builder, &request.tools)
            .map_err(|e| LlmError::Config(format!("mistralrs tool conversion: {e}")))?;
        if let Some(temp) = request.temperature {
            builder = builder.set_sampler_temperature(temp.into());
        }
        if let Some(max_tokens) = request.max_tokens {
            builder = builder.set_sampler_max_len(max_tokens as usize);
        }

        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(async move {
            match model.stream_chat_request(builder).await {
                Ok(stream) => {
                    forward_stream_via_mpsc(stream, tx, convert_response_to_stream_event).await;
                }
                Err(e) => {
                    let _ = tx
                        .send(Err(LlmError::Config(format!(
                            "mistralrs stream_chat_request: {e}"
                        ))))
                        .await;
                }
            }
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }
}
```

(The `todo!()` in `convert_response_to_stream_event` and the note above `apply_tools` are deliberate, explicit stop-and-verify markers pointing at the one place this plan could not fully ground without the real downloaded source — replace both before this task is considered done, using what Step 1 recorded. This is not the same as an open-ended "add appropriate handling" placeholder: the exact task — extract delta text from a named, specific field path — is stated, only the field path itself is deferred to real-source verification.)

- [ ] **Step 6: Fill in `convert_response_to_stream_event` from the real source**

Using what Step 1 recorded, replace the `todo!()` with the real extraction — for example, if the confirmed shape is `chunk.choices[0].delta.content: Option<String>` (typical of this class of streaming API; confirm before using):

```rust
        mistralrs::Response::Chunk(chunk) => {
            let delta = chunk
                .choices
                .first()
                .and_then(|choice| choice.delta.content.clone())
                .unwrap_or_default();
            Ok(StreamEvent::TextDelta(delta))
        }
```

Adjust field names exactly to match what Step 1 actually found — do not leave this as the placeholder shown above without confirming it compiles against the real crate.

- [ ] **Step 7: Create `mod.rs` and wire the module in**

Create `crates/aivyx-llm/src/mistral_rs/mod.rs`:

```rust
//! Embedded Rust-native inference via the `mistralrs` crate, ported
//! from aivyx's own Phase 134 -- with real token streaming from day
//! one (see `provider.rs`'s module doc for why aivyx's own version
//! shipped non-streaming and what this version does differently).

pub mod convert;
pub mod provider;

pub use provider::MistralRsBackend;
```

In `crates/aivyx-llm/src/lib.rs`, replace whatever temporary module declaration Task 3 added with:

```rust
#[cfg(feature = "provider-mistral-rs")]
pub mod mistral_rs;
```

- [ ] **Step 8: Run the full test suite with the feature enabled**

Run: `cargo test -p aivyx-llm --features provider-mistral-rs`
Expected: all tests pass, including the 8 conversion-layer tests from Task 3 and the 3 forwarding tests from this task.

- [ ] **Step 9: Confirm the default build is still unaffected**

Run: `cargo build -p aivyx-llm`
Expected: builds cleanly, `mistral_rs` module not compiled at all (feature-gated, not enabled by default).

- [ ] **Step 10: Commit**

```bash
git add crates/aivyx-llm/src/mistral_rs/ crates/aivyx-llm/src/lib.rs
git commit -m "feat: add MistralRsBackend with real token streaming

The mpsc-forwarding shim aivyx's own Phase 134 doc identified but never
built: mistral.rs's borrowed Stream<'_> is driven entirely inside a
spawned task holding its own Arc<Model> clone, forwarding converted
events through an mpsc channel wrapped as ReceiverStream -- genuinely
'static, no ouroboros or self-referential-struct dependency needed.
forward_stream_via_mpsc is generic over the source item type, tested
independently of any real GGUF model via a fake stream (this
environment has neither a real model nor a GPU)."
```

---

### Task 5: Dispatch in `agent_builder.rs`

**Files:**
- Modify: `crates/aivyx/src/agent_builder.rs`

**Interfaces:**
- Consumes: `BackendKind::MistralRs` and the 5 config fields (Task 2); `MistralRsBackend::new` (Task 4).
- Produces: nothing consumed by a later task (last task before docs).

- [ ] **Step 1: Write the failing test for the missing-model-path error**

Find this file's own test module (search for an existing test near the top-level construction logic, or add a new `#[cfg(test)] mod tests` block if none targets `agent_builder.rs` directly — check first with `grep -n "#\[cfg(test)\]" crates/aivyx/src/agent_builder.rs`). Add:

```rust
    #[tokio::test]
    #[cfg(feature = "provider-mistral-rs")]
    async fn mistral_rs_backend_construction_fails_clearly_without_model_path() {
        let mut settings = Settings::default();
        settings.backend.kind = BackendKind::MistralRs;
        // mistralrs_model_path deliberately left None.
        let result = build_llm_backend(&settings).await;
        let err = result.expect_err("must fail without a model path");
        assert!(
            err.to_string().contains("mistralrs_model_path"),
            "error should name the missing field, got: {err}"
        );
    }
```

(This assumes the dispatch logic in Step 3 below is extracted into a small, directly-testable `async fn build_llm_backend(settings: &Settings) -> anyhow::Result<Arc<dyn LlmBackend>>` rather than left inline in `build_agent` — do this extraction as part of Step 3, both because it's directly testable this way and because `build_agent` itself is already a large function per this repo's own established pattern of factoring out testable pieces.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p aivyx --features provider-mistral-rs mistral_rs_backend_construction_fails_clearly_without_model_path`
Expected: FAIL to compile — `build_llm_backend` doesn't exist yet.

- [ ] **Step 3: Extract the dispatch into `build_llm_backend` and implement it**

Find the current construction:

```rust
    let llm: Arc<dyn LlmBackend> = Arc::new(OpenAiCompatBackend::new(
        settings.backend.base_url.clone(),
        settings.backend.model.clone(),
        settings.backend.api_key.clone(),
    ));
```

Replace with:

```rust
    let llm: Arc<dyn LlmBackend> = build_llm_backend(settings).await?;
```

Add the new function elsewhere in the same file (e.g. just above `build_agent`):

```rust
/// Constructs the configured `LlmBackend`. Extracted from `build_agent`
/// so the dispatch itself -- including its error path when a required
/// mistral.rs config field is missing -- is directly testable without
/// building a full `BuiltAgent`.
async fn build_llm_backend(settings: &Settings) -> anyhow::Result<Arc<dyn LlmBackend>> {
    match settings.backend.kind {
        BackendKind::Generic | BackendKind::LlamaServer => Ok(Arc::new(OpenAiCompatBackend::new(
            settings.backend.base_url.clone(),
            settings.backend.model.clone(),
            settings.backend.api_key.clone(),
        ))),
        #[cfg(feature = "provider-mistral-rs")]
        BackendKind::MistralRs => {
            let model_path = settings.backend.mistralrs_model_path.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "backend.kind = \"mistral_rs\" but backend.mistralrs_model_path is missing. \
                     Set `mistralrs_model_path = \"/abs/path/to/model.gguf\"` under [backend] in \
                     your config."
                )
            })?;
            let backend = aivyx_llm::mistral_rs::MistralRsBackend::new(
                PathBuf::from(model_path),
                settings.backend.mistralrs_model_file.clone(),
                settings.backend.mistralrs_chat_template_path.clone().map(PathBuf::from),
                settings.backend.mistralrs_max_seq_len,
                settings.backend.mistralrs_constrain_tool_calls,
            )
            .await
            .map_err(|e| anyhow::anyhow!("failed to build mistralrs backend: {e}"))?;
            Ok(Arc::new(backend))
        }
        #[cfg(not(feature = "provider-mistral-rs"))]
        BackendKind::MistralRs => Err(anyhow::anyhow!(
            "backend.kind = \"mistral_rs\" but this binary was built without the \
             `provider-mistral-rs` feature. Rebuild with `cargo install --features \
             provider-mistral-rs aivyx-coder` to enable embedded inference."
        )),
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p aivyx --features provider-mistral-rs mistral_rs_backend_construction_fails_clearly_without_model_path`
Expected: 1 passed.

- [ ] **Step 5: Run the full suite both with and without the feature**

Run: `cargo test -p aivyx` (default, no feature)
Expected: passes, same count as before this task.

Run: `cargo test -p aivyx --features provider-mistral-rs`
Expected: passes, +1 for the new test.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx/src/agent_builder.rs
git commit -m "feat: dispatch to MistralRsBackend when backend.kind = mistral_rs

Extracts the LlmBackend construction into build_llm_backend so the
dispatch (including the actionable error when a required field is
missing, or when the binary was built without the provider-mistral-rs
feature) is directly testable. First time this codebase has ever
selected between two different LlmBackend implementations -- the
Generic/LlamaServer arm is unchanged."
```

---

### Task 6: Documentation

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: nothing (documentation only).
- Produces: nothing consumed by another task.

- [ ] **Step 1: Add the embedded-inference section**

Find the existing "KV-cache persistence" section in `README.md` (this repo documents everything in one `README.md`, not a separate `INSTALL.md` the way `aivyx` does). Add a new section immediately after it:

```markdown
### Embedded Rust-native inference

`aivyx-coder` can run a local LLM **inside its own process** by linking
against the `mistralrs` crate — the same capability `aivyx` (the sibling
Personal Assistant product) shipped in its own Phase 134, ported here
with real token streaming from the start. Zero outbound network calls
during inference; no separate runtime server to install.

#### Building with the embedded provider

```bash
# Lean build (default) — no mistralrs dependency, fast compile, small binary:
$ cargo install aivyx

# Embedded provider, CPU only — no C compiler, no CUDA toolkit, no Metal SDK required:
$ cargo install --features provider-mistral-rs aivyx

# Embedded provider with platform GPU acceleration — pick exactly one:
$ cargo install --features provider-mistral-rs-cuda aivyx       # NVIDIA
$ cargo install --features provider-mistral-rs-metal aivyx      # Apple Silicon
$ cargo install --features provider-mistral-rs-accelerate aivyx # Apple CPU
```

| Backend | Feature | Build prerequisite | Runtime |
|---|---|---|---|
| CPU | `provider-mistral-rs` | None | Any platform |
| CUDA | `provider-mistral-rs-cuda` | CUDA toolkit (>= 11.8) | NVIDIA GPU with CC >= 8.0 |
| Metal | `provider-mistral-rs-metal` | macOS + Xcode | Apple Silicon |
| Accelerate | `provider-mistral-rs-accelerate` | macOS + Xcode | Apple CPU |

#### `config.toml` snippet

```toml
[backend]
kind = "mistral_rs"
model = "qwen3-4b"  # display name; arbitrary string

# REQUIRED — absolute path to a GGUF file or a directory containing GGUF files.
mistralrs_model_path = "/home/you/models/Qwen3-4B-Q4_K_M.gguf"

# Optional — when mistralrs_model_path is a directory, names the specific file.
# mistralrs_model_file = "qwen3-4b-q4_k_m.gguf"

# Optional — chat template path. Omit to use the template embedded in the GGUF.
# mistralrs_chat_template_path = "/home/you/templates/qwen3.json"

# Optional — maximum sequence length. Omit to defer to the model's own default.
# mistralrs_max_seq_len = 32768
```

#### Recommended GGUF models

`aivyx-coder` doesn't bundle any model — download the GGUF yourself and
point `mistralrs_model_path` at it:

| Model | Size (Q4_K_M) | Min RAM | Use case | Download |
|---|---|---|---|---|
| **Qwen3-4B** | ~2.5GB | 6GB | Best general agent; strong tool calling | [HF: Qwen/Qwen3-4B-Instruct-GGUF](https://huggingface.co/Qwen) |
| **Llama-3.2-3B-Instruct** | ~2.0GB | 5GB | Conservative default; well-tested | [HF: bartowski/Llama-3.2-3B-Instruct-GGUF](https://huggingface.co/bartowski) |
| **Phi-4-mini-instruct** | ~2.4GB | 5GB | Microsoft tooling; XML tool-call format | [HF: microsoft/Phi-4-mini-instruct-gguf](https://huggingface.co/microsoft) |
| **SmolLM2-1.7B-Instruct** | ~1.1GB | 3GB | Smallest practical agent; CPU-friendly | [HF: HuggingFaceTB/SmolLM2-1.7B-Instruct-GGUF](https://huggingface.co/HuggingFaceTB) |

#### When to pick embedded vs. Ollama/llama-server

- **Pick embedded** for a single-binary install with no separate runtime
  to manage, for zero outbound network calls during inference, or when
  recommending `aivyx-coder` to an operator who'd otherwise stall at
  "install Ollama first."
- **Stick with Ollama/llama-server** if you want `ollama pull <model>`
  as your download UX, or you already have one running and aren't
  motivated to rebuild.

#### Honest tradeoffs

- **Build cost.** First build with `--features provider-mistral-rs`:
  ~5-10 minutes (mistralrs is a substantial crate; incremental builds
  after that are fast).
- **Binary size.** Release binary adds ~100-200MB on the CPU variant.
- **mistralrs is pre-1.0.** Pinned to `=0.8.*`; upgrades happen
  explicitly, matching aivyx's own upgrade-by-version contract.
- **TLS stack.** mistralrs's transitive dependencies pull in
  `aws-lc-rs` alongside this project's otherwise-`rustls`-only
  `reqwest` configuration. Both stacks coexist in an opt-in build; the
  default (no feature) build stays rustls-only.
- **Per-model tool-call format quirks are unverified.** Models with
  non-standard tool-call formats may behave differently through
  mistral.rs's own extraction than through Ollama — not yet empirically
  validated against a real model in this environment (which has neither
  a GPU nor a downloaded GGUF file to test against).
```

- [ ] **Step 2: Confirm the doc renders as valid markdown**

Run: `grep -n "Embedded Rust-native inference" README.md` and read the surrounding ~120 lines to confirm the section landed once, in the right place, with every code fence closed.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: document embedded mistral.rs inference

Mirrors aivyx's own INSTALL.md embedded-inference section structure
(build commands, backend prerequisite table, config snippet,
recommended GGUF models, when-to-pick guidance, honest tradeoffs),
adapted to this repo's own single-README convention and flat [backend]
config fields."
```

---

## Self-Review

**1. Spec coverage:** Spec section 1 (dependency/features) → Task 1. Section 2 (config) → Task 2. Section 3 (backend + streaming shim) → Tasks 3-4. Section 4 (dispatch) → Task 5. Section 5 (explicitly out of scope) → not implemented, correctly, and named as such in this plan's own task boundaries (no task builds Landlock confinement for the in-process engine, or per-model tool-call-quirk handling). Testing section → covered across Tasks 2-5's own test steps. No gaps.

**2. Placeholder scan:** One deliberate, explicit stop-and-verify marker exists (`todo!()` in Task 4 Step 5, immediately resolved in Step 6) — this is flagged inline as different from an open-ended placeholder: the exact extraction task is stated, only a field path unconfirmable without the real downloaded source is deferred, with an explicit follow-up step to close it before the task is done. No other TBD/TODO exists anywhere in this plan.

**3. Type consistency:** `MistralRsBackend::new`'s 5-parameter signature (Task 4) matches its call site in `build_llm_backend` (Task 5) exactly, parameter-for-parameter. `forward_stream_via_mpsc`'s generic signature (Task 4) is used consistently by both its own tests and the real `stream_chat` call site in the same task. `BackendKind::MistralRs` and the 5 `BackendSettings` field names (Task 2) are used identically in Task 5's dispatch — no drift.
