# Embedded Rust-Native Inference (`mistral.rs`) for aivyx-coder

**Status: approved, ready for implementation planning.**

## Context

`aivyx` (the flagship, sibling Personal Assistant product) shipped an
embedded, in-process GGUF inference backend in its own Phase 134
("Direction B: Embedded Rust-Native Inference"), linking against the
`mistralrs` crate so an operator can run a local model with zero
external server process and zero network calls. `aivyx-coder` has no
equivalent today — confirmed via a full grep of its workspace, zero
`mistralrs` references anywhere.

This phase ports that capability to `aivyx-coder`, but it is **not** a
pure copy-paste. Direct comparison against `aivyx`'s real, shipped code
(not just its design doc) found two things that make this a smaller
version of a first-of-its-kind change, not an extension of an existing
pattern:

- **`aivyx-coder`'s `LlmBackend` trait has had exactly one
  implementation, ever** (`OpenAiCompatBackend`, `crates/aivyx-llm/src/backend.rs`).
  `BackendKind::{Generic, LlamaServer}` only ever toggles *features* of
  that one struct (most recently, the kvcache override this session
  shipped) — it has never selected between two different concrete
  backends. `aivyx`, by contrast, already had a 4-provider dispatch
  (`ProviderKind::{Anthropic,OpenAI,Ollama,LlamaCpp,Jan}`) before Phase
  134 added a 5th arm. Adding `MistralRs` to `aivyx-coder` is the first
  time this codebase will ever pick between two `LlmBackend`
  implementations at runtime.
- **`aivyx-coder`'s workspace has zero existing Cargo feature flags**
  anywhere (confirmed: no `[features]` section in `aivyx-llm/Cargo.toml`
  or `crates/aivyx/Cargo.toml`). This phase introduces the first one.

Both are small, well-precedented additions — `aivyx`'s own Phase 134 is
a working, complete reference implementation to follow — but the spec
below is explicit about this being new ground for this codebase
specifically, not a mechanical port.

**The other real difference from `aivyx`'s Phase 134, and the one the
user explicitly chose to solve properly rather than repeat**: `aivyx`'s
own `MistralRsProvider` shipped non-streaming (mistral.rs 0.8's
`Stream<'a>` borrows from the `Model` with a lifetime that doesn't
satisfy the trait's `Send + 'static` bound; `aivyx`'s own Phase 134 doc
logged the fix — an mpsc-forwarding spawned task — as deliberately
deferred, and it was never picked up in any later phase). `aivyx-coder`
is a terminal-first product where visible token-by-token streaming is a
more central part of the UX than it likely is for `aivyx`'s daemon
architecture; shipping the same non-streaming compromise here would be
a more noticeable regression. This phase builds the mpsc-forwarding
shim `aivyx`'s own doc named but never built.

## Approach

### 1. Dependency and feature flags

Add to `crates/aivyx-llm/Cargo.toml`:

```toml
[dependencies]
mistralrs = { version = "=0.8.*", default-features = false, optional = true }

[features]
provider-mistral-rs = ["dep:mistralrs"]
provider-mistral-rs-cuda = ["provider-mistral-rs", "mistralrs?/cuda"]
provider-mistral-rs-metal = ["provider-mistral-rs", "mistralrs?/metal"]
provider-mistral-rs-accelerate = ["provider-mistral-rs", "mistralrs?/accelerate"]
```

Names deliberately mirror `aivyx`'s own feature names exactly — these
are sibling products under the same org; an operator or doc-reader
benefits from one consistent naming convention across both READMEs,
not two similar-but-different ones. Pinned to the same exact version
(`=0.8.*`) `aivyx` uses, for the same reason `aivyx`'s own doc gives:
mistral.rs is pre-1.0 and its API churns between minor versions.

No `recommended-providers` meta-feature yet — `aivyx-coder` has no
existing multi-feature bundling convention to fit one into (unlike
`aivyx`, which already had one before Phase 134 extended it). An
operator opts in explicitly with `--features provider-mistral-rs`
(plus a backend feature if they want GPU acceleration). A bundling
meta-feature can be added later if real usage shows it's wanted —
YAGNI for this phase.

Same TLS-stack risk `aivyx`'s own Phase 134 doc logged and accepted:
`mistralrs`'s transitive deps (`candle`, `hf-hub`) pull in `aws-lc-rs`,
while `aivyx-coder`'s own `reqwest` dependencies are configured
`rustls`-only (confirmed: `default-features = false, features = [...,
"rustls"]` in `aivyx-llm`/`aivyx`/`aivyx-tools`'s own `Cargo.toml`s).
Accepted for opt-in builds only, same posture `aivyx` took: the default
build (no `provider-mistral-rs` feature) stays rustls-only.

### 2. Config

`BackendKind` (`crates/aivyx-config/src/lib.rs`) gains a third variant:

```rust
pub enum BackendKind {
    #[default]
    Generic,
    LlamaServer,
    MistralRs,
}
```

New fields land as **flat fields directly on `BackendSettings`**,
matching this repo's own established convention (this is exactly how
`kvcache_store_path`/`kvcache_max_bytes` already sit directly in
`[backend]`) rather than `aivyx`'s own separate `[mistralrs]` TOML
table — each repo keeps its own idiom, deliberately not imported
wholesale from the other:

```rust
    /// Absolute path to a local GGUF file or model directory. Required
    /// when `kind = "mistral_rs"` — checked at backend-construction
    /// time, not at config-load time (mirrors how llama-server's own
    /// reachability isn't checked at load time either).
    pub mistralrs_model_path: Option<String>,
    /// Selects a specific `.gguf` file inside `mistralrs_model_path`
    /// when it's a directory containing more than one candidate.
    pub mistralrs_model_file: Option<String>,
    /// Overrides the chat template mistral.rs would otherwise infer
    /// from the model's own metadata.
    pub mistralrs_chat_template_path: Option<String>,
    /// Maximum sequence length mistral.rs allocates KV-cache space
    /// for. `None` lets mistral.rs pick its own default.
    pub mistralrs_max_seq_len: Option<usize>,
    /// Chapter Emboss/Stencil-equivalent: grammar-constrained
    /// tool-calling via mistral.rs's own JSON-schema constraint
    /// support. Default off, matching `aivyx`'s own default.
    #[serde(default)]
    pub mistralrs_constrain_tool_calls: bool,
```

`BackendKind::MistralRs`'s own doc comment gets a line explaining it
selects a genuinely different backend implementation, not a feature
toggle on `OpenAiCompatBackend` the way `LlamaServer` does — this
distinction matters for whoever next extends this enum.

### 3. The new backend + the streaming shim

New module, `crates/aivyx-llm/src/mistral_rs/` (mirroring `aivyx`'s own
module layout: `mod.rs`, `provider.rs`, `convert.rs`), gated entirely
behind `#[cfg(feature = "provider-mistral-rs")]`:

- **`convert.rs`**: pure conversion functions between `aivyx-coder`'s
  own `aivyx_types::{Message, ToolCall, ToolDefinition}` and
  mistral.rs's `TextMessages`/tool-call shapes — structurally the same
  job as `aivyx`'s own `convert.rs`, adapted to this repo's own types.
- **`provider.rs`**: `MistralRsBackend`, implementing `LlmBackend`.
  Loads the GGUF model at construction time via mistral.rs's
  `GgufModelBuilder`, keeping the resulting handle as `Arc<Model>` —
  same shape `aivyx`'s own `MistralRsProvider` already uses, and the
  reason this shim is possible cheaply: `Arc<Model>` clones are cheap,
  so a clone can be moved into a spawned task without disturbing the
  backend's own copy.

**The streaming shim** — `stream_chat`'s real implementation:

```rust
async fn stream_chat(
    &self,
    request: ChatRequest,
) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
    let model = Arc::clone(&self.model);
    let builder = build_request(&request)?; // convert.rs
    let (tx, rx) = tokio::sync::mpsc::channel(32);

    tokio::spawn(async move {
        // The borrowed `Stream<'a>` mistral.rs returns here borrows
        // from *this task's own* `model` clone, which lives exactly
        // as long as this task does -- the lifetime is fully
        // satisfied inside this scope. Nothing borrowed ever crosses
        // back out of the task; only owned `StreamEvent`s do, over
        // the channel.
        let mut stream = match model.stream_chat_request(builder).await {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(Err(LlmError::Config(format!(
                    "mistralrs stream_chat_request: {e}"
                )))).await;
                return;
            }
        };
        while let Some(chunk) = stream.next().await {
            let event = convert_chunk_to_stream_event(chunk); // convert.rs
            // `tx.send` fails only once the receiver (the caller's
            // stream) has been dropped -- the caller lost interest
            // (turn cancelled, agent dropped). Stop driving inference
            // immediately rather than continuing to burn CPU/GPU on a
            // turn nobody is reading anymore.
            if tx.send(event).await.is_err() {
                break;
            }
        }
    });

    Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
}
```

`tokio_stream` is already available transitively (or added as a small,
already-widely-used direct dependency if not) — `ReceiverStream` is the
standard, minimal wrapper turning an `mpsc::Receiver` into a `Stream`;
no new heavyweight dependency needed, and no `ouroboros`-style
self-referential struct required either. This is the fix `aivyx`'s own
Phase 134 doc named as one of the two options and never built —
closing an already-identified gap, not inventing a new approach.

`LlmBackend::stream_chat`'s own trait signature is unchanged; no other
backend or call site needs to change. `aivyx-coder` doesn't have a
`CancellationToken` parameter on this trait (unlike `aivyx`'s
`LlmProvider::chat_stream`) — cancellation here works the same way it
already does for every other backend in this codebase: the caller
drops the stream, and (per the code above) the spawned task notices on
its next `tx.send` and stops.

### 4. Dispatch

`crates/aivyx/src/agent_builder.rs`'s single hardcoded construction:

```rust
let llm: Arc<dyn LlmBackend> = Arc::new(OpenAiCompatBackend::new(
    settings.backend.base_url.clone(),
    settings.backend.model.clone(),
    settings.backend.api_key.clone(),
));
```

becomes a real dispatch — the first of its kind in this codebase:

```rust
let llm: Arc<dyn LlmBackend> = match settings.backend.kind {
    BackendKind::Generic | BackendKind::LlamaServer => {
        Arc::new(OpenAiCompatBackend::new(
            settings.backend.base_url.clone(),
            settings.backend.model.clone(),
            settings.backend.api_key.clone(),
        ))
    }
    #[cfg(feature = "provider-mistral-rs")]
    BackendKind::MistralRs => {
        let model_path = settings.backend.mistralrs_model_path.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "backend.kind = \"mistral_rs\" but backend.mistralrs_model_path is missing"
            )
        })?;
        Arc::new(aivyx_llm::mistral_rs::MistralRsBackend::new(/* ... */).await?)
    }
    #[cfg(not(feature = "provider-mistral-rs"))]
    BackendKind::MistralRs => {
        anyhow::bail!(
            "backend.kind = \"mistral_rs\" but this binary was built without the \
             `provider-mistral-rs` feature. Rebuild with `cargo install --features \
             provider-mistral-rs aivyx-coder` to enable embedded inference."
        );
    }
};
```

Matches `aivyx`'s own precedent for the actionable-error-when-feature-missing
UX exactly.

### 5. Explicitly out of scope (named, not silently skipped)

- **Landlock/seccomp confinement does not apply.** It wraps spawned
  child processes only (`run_command`/`run_shell`); an in-process
  inference engine runs at the same trust level as the rest of the
  `aivyx-coder` binary. Same posture `aivyx` accepted for its own
  embedded provider — no new sandboxing question this phase needs to
  answer.
- **Tool-call format quirks per model family** (e.g. `aivyx`'s own doc
  flagged qwen3.x's XML wrappers, phi4-mini's `<tool_call>` blocks
  behaving differently through mistral.rs's own extraction vs. through
  Ollama) are logged as needing empirical validation once a real model
  is tested against this backend — not solved speculatively now,
  matching `aivyx`'s own honest scope framing for the same risk.
- **No bundled/auto-downloaded models.** Same as `aivyx`'s Q3a
  decision: document recommended GGUF models in `README.md`, operator
  supplies the path. Strongest privacy posture, clearest consent.
- **Multimodal (image) input** stays out of scope, same as `aivyx`'s
  own Phase 134 MVP boundary.

## Testing

Mirrors `aivyx`'s own Phase 134 test additions, adapted to this repo's
types:

- `BackendKind`/`BackendSettings` parse + validate + `Default` tests:
  `mistral_rs` parses from TOML (`kind = "mistral_rs"`), the four new
  optional fields round-trip, `mistralrs_model_path` is required only
  when `kind == MistralRs` (checked at the dispatch call site per
  section 4, not at config-parse time — a config test should confirm
  parsing itself never fails just because the field is absent).
- `convert.rs` conversion-layer unit tests: `aivyx_types::Message` →
  mistral.rs message shape and back, `ToolDefinition`/`ToolCall`
  round-trips — mirroring `aivyx`'s own 5 conversion-layer tests.
- **New tests this phase adds that `aivyx`'s own Phase 134 didn't need
  (because it shipped non-streaming)**: the mpsc-forwarding shim itself
  needs direct testing, independent of a real mistral.rs model —
  construct a fake stream of chunks, drive it through the same
  spawn-and-forward logic, and assert: (a) events arrive on the
  `ReceiverStream` in order, (b) dropping the receiver early causes the
  spawned task to stop sending (observable via a counter/flag the fake
  stream increments on each poll, asserting it stops growing once the
  receiver is dropped), (c) an error from the upstream stream is
  forwarded as a `Result::Err` on the channel rather than silently
  dropped or panicking.
- Full existing suite re-run with and without `--features
  provider-mistral-rs` to confirm the default (no-feature) build is
  completely unaffected.

## Self-review

- **Placeholder scan:** none — every new field, function signature, and
  dispatch arm is given concretely. The dispatch snippet's `/* ... */`
  for `MistralRsBackend::new`'s full argument list is deliberately left
  for the plan to fill in once it reads `MistralRsBackend`'s own real
  constructor signature (defined in the same plan, section 3) — not a
  spec-level gap, a forward-reference within the same document.
- **Internal consistency:** the flat-fields-in-`[backend]` config
  decision (section 2) is consistent with this repo's existing
  `kvcache_*` fields; the feature-flag naming (section 1) is
  consistent with `aivyx`'s own convention, a deliberate cross-repo
  choice stated explicitly rather than left ambiguous.
- **Scope check:** one repo, one crate's worth of new code
  (`aivyx-llm`) plus small edits to `aivyx-config` and `agent_builder.rs`
  — sized for a small number of implementation tasks, not a
  decomposition into separate specs.
- **Ambiguity check:** the streaming-vs-non-streaming call was
  confirmed directly with the user rather than assumed (the more
  involved option, chosen explicitly over the faster/lower-risk one).
  The config-shape choice (flat fields vs. a new TOML table) is stated
  as a deliberate convention-following decision, not left for the plan
  to improvise.
