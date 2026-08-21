# `aivyx-kvcache` Adoption in `aivyx-coder` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `aivyx-coder` persist and restore local-model KV-cache state across process restarts when talking to `llama-server`, using the now-e2e-tested `aivyx-kvcache` crate — opt-in, zero behavior change for every other backend.

**Architecture:** A new `[backend] kind = "llama_server"` config gate; a pure numeric `KvSlotPool` (checkout/release, mirrors `aivyx-mcp-server`'s `SessionMap::take`/`put_back` shape); `id_slot` threaded as a new optional field through `ChatRequest`/`WireRequest`; `total_slots`/`build_info` read from the existing `/props` probe (no new HTTP round trip); the actual save/restore/warm-up policy lives in `aivyx-core::Agent` (not `aivyx-llm`) since that's where the real "stable prefix" — system prompt + tool defs + repo map — is actually assembled, and where a session's natural lifetime (one `Agent` instance) already exists to hang checkout-at-start/release-on-`Drop` off of.

**Tech Stack:** Rust, `aivyx-kvcache` (new git dependency, pinned rev `ee69d531c1e72dd6ea455c6554f37b010d69e981`), no other new dependencies.

## Global Constraints

- `[backend] kind` defaults to unset/`Generic` — every existing config keeps today's exact behavior with zero migration. Only `kind = "llama_server"` opts in.
- Every kvcache operation is fail-open: a `--slot-save-path`-less server, a network error, a full pool — none of it may ever surface as a turn failure. Log at `warn`, proceed as if kvcache weren't configured.
- The saved/restored KV state must never include real conversation content — `CacheKey.prefix_hash` covers only system prompt + tool defs + repo map, and the disk write (`save_from_slot`) happens **at most once per distinct key, ever**, via a dedicated warm-up call that never touches `self.history`. Re-saving on every session/turn is a correctness bug (a later, unrelated session would restore stale conversation under the same key), not just a missed optimization — do not do it.
- `id_slot` is pinned on every real request of a session once a slot is checked out (from the first real turn onward), not just the warm-up call — this is what keeps the automatic `cache_prompt` matching extending correctly turn to turn.
- The `aivyx-kvcache` API surface used here (verified against real source, `aivyx-kvcache` rev `ee69d531c1e72dd6ea455c6554f37b010d69e981`):
  - `LlamaServerSlotStore::open(store_path: impl Into<PathBuf>, base_url: impl Into<String>, max_bytes: u64) -> Result<Self, KvCacheError>`
  - `LlamaServerSlotStore::save_from_slot(&self, key: &CacheKey, slot_id: u32, meta: CacheMeta) -> Result<(), KvCacheError>`
  - `LlamaServerSlotStore::restore_into_slot(&self, key: &CacheKey, slot_id: u32) -> Result<bool, KvCacheError>` (never `Err` on a plain cache miss)
  - `KvCacheStore::find(&self, key: &CacheKey) -> Result<Option<CacheHandle>, KvCacheError>` (trait method — `use aivyx_kvcache::KvCacheStore;`)
  - `CacheKey { backend_id: String, model_id: String, build_hash: String, prefix_hash: String }`, `CacheMeta { size_bytes: u64, token_count: u64 }`

---

### Task 1: `[backend] kind` config field

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs`
- Test: `crates/aivyx-config/src/lib.rs` (inline `#[cfg(test)]` module — this crate's existing convention)

**Interfaces:**
- Produces: `pub enum BackendKind { Generic, LlamaServer }` (default `Generic`) and a new `pub kind: BackendKind` field on `BackendSettings`. Later tasks read `settings.backend.kind == BackendKind::LlamaServer` to decide whether to wire the kvcache path at all.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)]` module in `crates/aivyx-config/src/lib.rs` (near the existing `BackendSettings`-related tests):

```rust
    #[test]
    fn backend_kind_defaults_to_generic_when_absent() {
        let settings: Settings = toml::from_str("").unwrap();
        assert_eq!(settings.backend.kind, BackendKind::Generic);
    }

    #[test]
    fn backend_kind_parses_llama_server() {
        let raw = r#"
            [backend]
            base_url = "http://127.0.0.1:8080/v1"
            model = "test-model"
            kind = "llama_server"
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert_eq!(settings.backend.kind, BackendKind::LlamaServer);
    }
```

(`toml::from_str::<Settings>(...)` — this crate's real, direct pattern; `Settings` derives `Deserialize` on its own, no intermediate raw/parse step. Verified against this file's own existing tests, e.g. `crates/aivyx-config/src/lib.rs:1020`.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-config backend_kind -- --test-threads=1`
Expected: FAIL — `BackendKind` doesn't exist yet, `kind` field doesn't exist on `BackendSettings`.

- [ ] **Step 3: Add `BackendKind` and wire it into `BackendSettings`**

Add near `EditFormat`/`ToolCallingMode` (same file, `crates/aivyx-config/src/lib.rs`):

```rust
/// Phase kvcache-adoption — which local-LLM backend server this config
/// talks to. `Generic` (the default) is today's fully backend-agnostic
/// behavior; `LlamaServer` opts a config into llama-server-specific
/// features (currently: KV-cache persistence via `aivyx-kvcache`, gated
/// on this exact variant so no other backend is ever affected).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    #[default]
    Generic,
    LlamaServer,
}
```

Add the field to `BackendSettings` (after `edit_format`):

```rust
    pub edit_format: EditFormat,
    /// Which backend server this config talks to — see `BackendKind`'s
    /// own doc comment. Default `Generic`: no behavior change for
    /// existing configs.
    pub kind: BackendKind,
```

And to its `impl Default for BackendSettings` block (after `edit_format: EditFormat::Native,`):

```rust
            kind: BackendKind::Generic,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-config -- --test-threads=1`
Expected: PASS, including the two new tests, no regressions in the rest of the crate's suite.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-config/src/lib.rs
git commit -m "feat: add [backend] kind config field (Generic | LlamaServer)"
```

---

### Task 2: `KvSlotPool` — a pure numeric checkout/release pool

**Files:**
- Create: `crates/aivyx-llm/src/kv_slot_pool.rs`
- Modify: `crates/aivyx-llm/src/lib.rs` (add `mod kv_slot_pool; pub use kv_slot_pool::KvSlotPool;`)

**Interfaces:**
- Produces: `pub struct KvSlotPool` with `pub fn new(total_slots: u32) -> Self`, `pub fn checkout(&self) -> Option<u32>`, `pub fn release(&self, slot_id: u32)`. Thread-safe (`Mutex`-backed) — later tasks share one instance via `Arc<KvSlotPool>` across every concurrent session in a process.
- This is a **pure id pool** — no I/O, no `aivyx-kvcache` dependency at all. It only tracks which of `0..total_slots` are currently checked out. The save/restore/warm-up policy that decides *what* to do with a checked-out slot id lives entirely in Task 5.

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-llm/src/kv_slot_pool.rs`:

```rust
//! A pure numeric slot-id pool -- tracks which of `0..total_slots` are
//! currently checked out. No I/O, no knowledge of `aivyx-kvcache` at all;
//! `aivyx-core::Agent` (the caller) decides what a checked-out slot id is
//! actually used for. Mirrors `aivyx-mcp-server`'s `SessionMap::take`/
//! `put_back` checkout/release shape.

use std::collections::HashSet;
use std::sync::Mutex;

pub struct KvSlotPool {
    total_slots: u32,
    checked_out: Mutex<HashSet<u32>>,
}

impl KvSlotPool {
    pub fn new(total_slots: u32) -> Self {
        Self {
            total_slots,
            checked_out: Mutex::new(HashSet::new()),
        }
    }

    /// Returns the lowest-numbered free slot id, or `None` if every slot
    /// is already checked out. Deterministic ordering (lowest-first)
    /// makes pool behavior predictable in tests; no particular ordering
    /// is required for correctness.
    pub fn checkout(&self) -> Option<u32> {
        let mut checked_out = self.checked_out.lock().unwrap();
        for id in 0..self.total_slots {
            if checked_out.insert(id) {
                return Some(id);
            }
        }
        None
    }

    /// Returns `slot_id` to the pool. A `slot_id` that was never checked
    /// out (or already released) is a silent no-op -- release is called
    /// from `Drop` impls, where panicking or erroring is not an option.
    pub fn release(&self, slot_id: u32) {
        self.checked_out.lock().unwrap().remove(&slot_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_returns_lowest_free_id_first() {
        let pool = KvSlotPool::new(4);
        assert_eq!(pool.checkout(), Some(0));
        assert_eq!(pool.checkout(), Some(1));
    }

    #[test]
    fn checkout_returns_none_once_the_pool_is_full() {
        let pool = KvSlotPool::new(2);
        assert_eq!(pool.checkout(), Some(0));
        assert_eq!(pool.checkout(), Some(1));
        assert_eq!(pool.checkout(), None, "pool of size 2 must reject a third concurrent checkout");
    }

    #[test]
    fn release_makes_a_slot_available_again() {
        let pool = KvSlotPool::new(1);
        let id = pool.checkout().expect("pool of size 1 has a free slot");
        assert_eq!(pool.checkout(), None, "the only slot is already checked out");
        pool.release(id);
        assert_eq!(pool.checkout(), Some(id), "release must make the slot checkoutable again");
    }

    #[test]
    fn releasing_a_never_checked_out_id_is_a_silent_no_op() {
        let pool = KvSlotPool::new(4);
        pool.release(99); // never checked out -- must not panic
        assert_eq!(pool.checkout(), Some(0), "pool must still function normally after a no-op release");
    }
}
```

- [ ] **Step 2: Wire the module in and run the tests**

Add to `crates/aivyx-llm/src/lib.rs` (alongside the existing `mod`/`pub use` lines):

```rust
mod kv_slot_pool;
pub use kv_slot_pool::KvSlotPool;
```

Run: `cargo test -p aivyx-llm kv_slot_pool -- --test-threads=1`
Expected: PASS, 4/4 new tests.

- [ ] **Step 3: Commit**

```bash
git add crates/aivyx-llm/src/kv_slot_pool.rs crates/aivyx-llm/src/lib.rs
git commit -m "feat: add KvSlotPool, a pure numeric slot checkout/release pool"
```

---

### Task 3: Thread `id_slot` through `ChatRequest`/`WireRequest`

**Files:**
- Modify: `crates/aivyx-llm/src/backend.rs`
- Modify: `crates/aivyx-llm/src/openai_compat.rs`

**Interfaces:**
- Consumes: `ChatRequest` (`crates/aivyx-llm/src/backend.rs:20`), `WireRequest`/`WireRequest::from_chat_request` (`crates/aivyx-llm/src/openai_compat.rs:310`+).
- Produces: `ChatRequest.id_slot: Option<u32>` (new field) — Task 5 sets this on every request of a session once a slot is checked out. `None` (the existing behavior for every non-llama-server backend, and for llama-server before checkout) omits the field from the wire request entirely.

- [ ] **Step 1: Write the failing test**

Add to `crates/aivyx-llm/src/openai_compat.rs`'s existing `#[cfg(test)]` module (find it — this file already has wire-format tests; add alongside them):

```rust
    #[test]
    fn wire_request_omits_id_slot_when_none() {
        let mut request = ChatRequest::new(vec![]);
        request.id_slot = None;
        let wire = WireRequest::from_chat_request("test-model", &request);
        let json = serde_json::to_value(&wire).unwrap();
        assert!(json.get("id_slot").is_none(), "id_slot must be omitted entirely when None");
    }

    #[test]
    fn wire_request_includes_id_slot_when_set() {
        let mut request = ChatRequest::new(vec![]);
        request.id_slot = Some(2);
        let wire = WireRequest::from_chat_request("test-model", &request);
        let json = serde_json::to_value(&wire).unwrap();
        assert_eq!(json.get("id_slot"), Some(&serde_json::json!(2)));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-llm wire_request_.*id_slot -- --test-threads=1`
Expected: FAIL — `ChatRequest` has no `id_slot` field yet.

- [ ] **Step 3: Add the field to `ChatRequest` and `WireRequest`**

In `crates/aivyx-llm/src/backend.rs`, add to `ChatRequest` (after `max_tokens`):

```rust
    pub max_tokens: Option<u32>,
    /// llama-server-only: pins this request to a specific `/slots` id
    /// (an extension beyond the OpenAI spec, but honored by llama-server
    /// on `/v1/chat/completions` — verified empirically against a real
    /// server, not documented in llama-server's own API reference). Only
    /// ever set when `[backend] kind = "llama_server"` and a slot has
    /// been checked out (see `aivyx-core::Agent`); `None` for every other
    /// backend and every llama-server request before checkout.
    pub id_slot: Option<u32>,
```

And to `ChatRequest::new`'s constructor (after `max_tokens: None,`):

```rust
            id_slot: None,
```

In `crates/aivyx-llm/src/openai_compat.rs`, add to `WireRequest` (after `max_tokens`):

```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id_slot: Option<u32>,
```

And to `WireRequest::from_chat_request` (after `max_tokens: request.max_tokens,`):

```rust
            id_slot: request.id_slot,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-llm -- --test-threads=1`
Expected: PASS, including the two new tests, no regressions.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-llm/src/backend.rs crates/aivyx-llm/src/openai_compat.rs
git commit -m "feat: thread id_slot through ChatRequest/WireRequest"
```

---

### Task 4: Extend the `/props` probe to also read `total_slots` and `build_info`

**Files:**
- Modify: `crates/aivyx-llm/src/probe.rs`

**Interfaces:**
- Consumes: the existing `probe_served_context(base_url: &str, model: &str) -> ServedContext` machinery — this task adds a second, small function reusing the same already-fetched `/props` JSON shape (llama-server's own response), not a second HTTP call from a caller's perspective when both are needed together (Task 6 calls both against the one JSON body it already has — see that task).
- Produces: `pub fn parse_llama_slots_info(json: &serde_json::Value) -> Option<LlamaSlotsInfo>` and `pub struct LlamaSlotsInfo { pub total_slots: u32, pub build_info: String }`. `None` for a non-llama-server `/props` response (a server that doesn't expose these fields, or doesn't expose `/props` at all).

- [ ] **Step 1: Write the failing tests**

Add to `crates/aivyx-llm/src/probe.rs`'s existing `#[cfg(test)]` module (it already has `parses_llama_server_props`, per `crates/aivyx-llm/src/probe.rs:112` — add alongside it):

```rust
    #[test]
    fn parses_llama_slots_info_from_real_props_shape() {
        // Real /props response shape, confirmed against a live llama-server
        // on the GPU test rig (2026-08-21) -- trimmed to the fields this
        // parser reads plus enough surrounding structure to be realistic.
        let json = serde_json::json!({
            "default_generation_settings": {"params": {}},
            "total_slots": 4,
            "model_path": "/home/julian/models/Qwen3.5-9B-Q4_K_M.gguf",
            "build_info": "b10107-3121043"
        });
        let info = parse_llama_slots_info(&json).expect("must parse a real llama-server /props body");
        assert_eq!(info.total_slots, 4);
        assert_eq!(info.build_info, "b10107-3121043");
    }

    #[test]
    fn parse_llama_slots_info_returns_none_when_fields_are_absent() {
        let json = serde_json::json!({"some_other_server": true});
        assert!(parse_llama_slots_info(&json).is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-llm parse_llama_slots_info -- --test-threads=1`
Expected: FAIL — `parse_llama_slots_info`/`LlamaSlotsInfo` don't exist yet.

- [ ] **Step 3: Add the parser**

Add to `crates/aivyx-llm/src/probe.rs` (after `parse_llama_props`):

```rust
/// `total_slots` + `build_info` from a real llama-server `/props`
/// response -- the same JSON body `probe_served_context` already fetches
/// for context-window detection, so a caller wanting both should parse
/// the one response with both this function and `parse_llama_props`
/// rather than fetching `/props` twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlamaSlotsInfo {
    pub total_slots: u32,
    pub build_info: String,
}

pub fn parse_llama_slots_info(json: &serde_json::Value) -> Option<LlamaSlotsInfo> {
    let total_slots = json.get("total_slots")?.as_u64()? as u32;
    let build_info = json.get("build_info")?.as_str()?.to_string();
    Some(LlamaSlotsInfo { total_slots, build_info })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-llm -- --test-threads=1`
Expected: PASS, no regressions.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-llm/src/probe.rs
git commit -m "feat: parse total_slots/build_info from the existing /props probe"
```

---

### Task 5: The checkout/warm-up/restore/pin/release policy in `Agent`

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs`
- Modify: `crates/aivyx-core/Cargo.toml` (add `aivyx-kvcache` dependency)

**Interfaces:**
- Consumes: `KvSlotPool` (Task 2), `ChatRequest.id_slot` (Task 3), `aivyx_kvcache::{CacheKey, CacheMeta, KvCacheStore, LlamaServerSlotStore}`.
- Produces: `Agent::set_kv_cache(&mut self, pool: Arc<KvSlotPool>, store: Arc<LlamaServerSlotStore>, backend_id: String, model_id: String, build_hash: String)` — a new setter, called only by Task 6's `agent_builder.rs` wiring when `[backend] kind = "llama_server"`. Absent this call, every code path added in this task is a complete no-op (matching every other optional `Agent` feature's own established shape, e.g. `set_repo_map`).

**A note on the one non-obvious finding from verifying this against real code:** `run_turn_inner` (`crates/aivyx-core/src/agent/mod.rs:1286`) pushes the user's new message onto `self.history` at line 1292 — *before* `refresh_repo_map`/`refresh_agents_files`/`refresh_editor_context` run at lines 1296-1298. This means the existing `assemble_messages()` (which includes `self.history`) can never be reused for the warm-up call without first stripping out that turn's real user message — fragile and easy to regress. Instead, this task extracts the *history-free* portion of `assemble_messages()`'s system-string construction into its own helper, `system_prompt_text()`, and the warm-up path calls that helper directly, never touching `self.history` at all. This is why the plan does this refactor rather than reusing `assemble_messages()` as-is.

- [ ] **Step 1: Add the `aivyx-kvcache` dependency**

In `crates/aivyx-core/Cargo.toml`, add to `[dependencies]` (matching the existing `aivyx-checkpoint`/`aivyx-confine` git-dependency convention in this workspace — see `crates/aivyx-tools/Cargo.toml:8`, `crates/aivyx-sandbox/Cargo.toml:8`):

```toml
aivyx-kvcache = { git = "https://github.com/Aivyx-Agent/aivyx-kvcache", rev = "ee69d531c1e72dd6ea455c6554f37b010d69e981" }
```

Run: `cargo build -p aivyx-core` — expected: succeeds, fetching the new dependency, no code changes needed yet for this to compile.

- [ ] **Step 2: Write the failing tests for `system_prompt_text()` and `compute_prefix_hash()`**

These are pure functions, easy to unit test without any real HTTP. Add to `crates/aivyx-core/src/agent/tests.rs` (this crate's existing test module for `Agent`):

```rust
#[test]
fn system_prompt_text_excludes_history() {
    // build_agent (this file's real test constructor, see build_agent/
    // build_agent_with_config above) needs no scripted responses for
    // these two tests -- they only construct an Agent and inspect its
    // own fields, never call run_turn.
    let (mut agent, _rx, _mock) = build_agent(vec![], ToolRegistry::new(), 5);
    agent.history.push(Message::text(Role::User, "a real user message"));
    let text = agent.system_prompt_text();
    assert!(
        !text.contains("a real user message"),
        "system_prompt_text must never include conversation history"
    );
}

#[test]
fn system_prompt_text_includes_repo_map_when_set() {
    let (mut agent, _rx, _mock) = build_agent(vec![], ToolRegistry::new(), 5);
    agent.repo_map_text = Some("## Repo Map\nfoo.rs: fn bar()".to_string());
    let text = agent.system_prompt_text();
    assert!(text.contains("## Repo Map"));
}

#[test]
fn compute_prefix_hash_is_stable_for_identical_inputs() {
    let tools = vec![ToolDefinition {
        name: "read_file".to_string(),
        description: "reads a file".to_string(),
        parameters_schema: serde_json::json!({"type": "object"}),
    }];
    let h1 = compute_prefix_hash("system prompt text", &tools);
    let h2 = compute_prefix_hash("system prompt text", &tools);
    assert_eq!(h1, h2);
}

#[test]
fn compute_prefix_hash_differs_when_system_text_differs() {
    let tools: Vec<ToolDefinition> = vec![];
    let h1 = compute_prefix_hash("prompt A", &tools);
    let h2 = compute_prefix_hash("prompt B", &tools);
    assert_ne!(h1, h2);
}

#[test]
fn compute_prefix_hash_differs_when_tools_differ() {
    let system = "same system text";
    let tools_a = vec![ToolDefinition {
        name: "read_file".to_string(),
        description: "reads".to_string(),
        parameters_schema: serde_json::json!({}),
    }];
    let tools_b = vec![ToolDefinition {
        name: "write_file".to_string(),
        description: "writes".to_string(),
        parameters_schema: serde_json::json!({}),
    }];
    assert_ne!(compute_prefix_hash(system, &tools_a), compute_prefix_hash(system, &tools_b));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p aivyx-core system_prompt_text compute_prefix_hash -- --test-threads=1`
Expected: FAIL — neither function exists yet.

- [ ] **Step 4: Refactor `assemble_messages()` to extract `system_prompt_text()`, and add `compute_prefix_hash()`**

In `crates/aivyx-core/src/agent/mod.rs`, replace the existing `assemble_messages` method (lines 645-689) with:

```rust
    fn system_prompt_text(&self) -> String {
        let mut system = self.system_prompt.clone();
        if self.history_truncated {
            system.push_str(
                "\n\n(Note: earlier parts of this conversation were truncated to fit the \
                 model's context window. Ask the user to restate anything you're missing.)",
            );
        }
        if self.plan_mode.active() {
            system.push_str("\n\n");
            system.push_str(PLAN_MODE_PROMPT);
        } else if self.edit_format == EditFormat::Prompted {
            system.push_str("\n\n");
            system.push_str(EDIT_FORMAT_PROMPT);
        }
        if self.verification.is_some() && !self.plan_mode.active() {
            system.push_str("\n\n");
            system.push_str(VERIFICATION_PROMPT);
        }
        if self.autonomous_mode.active() {
            system.push_str("\n\n");
            system.push_str(AUTONOMOUS_PROMPT);
        }
        if let Some(text) = &self.agents_files_text {
            system.push_str("\n\n");
            system.push_str(text);
        }
        if let Some(map) = &self.repo_map_text {
            system.push_str("\n\n");
            system.push_str(map);
        }
        if let Some(text) = &self.editor_context_text {
            system.push_str("\n\n");
            system.push_str(text);
        }
        system
    }

    fn assemble_messages(&self) -> Vec<Message> {
        let mut messages = Vec::with_capacity(self.history.len() + 1);
        messages.push(Message::text(Role::System, self.system_prompt_text()));
        messages.extend(self.history.iter().cloned());
        messages
    }
```

Add `compute_prefix_hash` as a free function near the bottom of the file, alongside the existing `message_chars` helper (`crates/aivyx-core/src/agent/mod.rs:1785`):

```rust
/// A stable-within-one-process-run hash of the stable prefix (system
/// prompt text + tool definitions) -- used as `CacheKey.prefix_hash`.
/// Deliberately NOT guaranteed stable across Rust versions/compilations:
/// a rebuild changing the hash algorithm just means old kvcache entries
/// silently miss instead of hit (fail-open, matching every other kvcache
/// operation in this integration), never a correctness problem.
fn compute_prefix_hash(system_text: &str, tools: &[ToolDefinition]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    system_text.hash(&mut hasher);
    for tool in tools {
        tool.name.hash(&mut hasher);
        tool.description.hash(&mut hasher);
        tool.parameters_schema.to_string().hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p aivyx-core -- --test-threads=1`
Expected: PASS, including the 5 new tests. This step also re-runs every existing `Agent` test, which is the real regression check for the `assemble_messages` refactor — confirm none of them fail (the refactor is behavior-preserving: `assemble_messages()` produces byte-identical output to before, just via the new helper).

- [ ] **Step 6: Commit the refactor + hash function on their own**

```bash
git add crates/aivyx-core/Cargo.toml crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/src/agent/tests.rs
git commit -m "refactor: extract system_prompt_text(), add compute_prefix_hash()"
```

- [ ] **Step 7: Add the `KvCache` field, setter, checkout/warm-up/restore logic, and `Drop`**

Add near the top of `crates/aivyx-core/src/agent/mod.rs` (with the other imports):

```rust
use aivyx_kvcache::{CacheKey, CacheMeta, KvCacheStore, LlamaServerSlotStore};
use aivyx_llm::KvSlotPool;
```

Add a new private struct (near `Agent`'s own definition) and a new `Agent` field:

```rust
struct KvCacheConfig {
    pool: Arc<KvSlotPool>,
    store: Arc<LlamaServerSlotStore>,
    backend_id: String,
    model_id: String,
    build_hash: String,
}
```

Add to the `Agent` struct (after `repo_map`):

```rust
    /// `None` unless `set_kv_cache` was called (only ever true when
    /// `[backend] kind = "llama_server"`) -- every other code path this
    /// task adds is a complete no-op when this is `None`.
    kv_cache: Option<KvCacheConfig>,
    /// The slot id checked out from `kv_cache`'s pool, once
    /// `ensure_kv_slot_checked_out` has run at least once successfully
    /// (or attempted to -- `None` also covers "the pool was full" and
    /// "no kv_cache configured", both of which mean every subsequent
    /// `ChatRequest.id_slot` for this session stays `None`, i.e. today's
    /// unpinned behavior).
    kv_slot_id: Option<u32>,
```

Add to `Agent::new`'s constructor body (wherever `repo_map: None,` is initialized):

```rust
            kv_cache: None,
            kv_slot_id: None,
```

Add the setter and the checkout/warm-up/restore method (near `set_repo_map`, `crates/aivyx-core/src/agent/mod.rs:413`):

```rust
    /// Opts this `Agent` into KV-cache persistence against a llama-server
    /// backend. Only ever called by `agent_builder.rs` when `[backend]
    /// kind = "llama_server"` -- every other backend never calls this,
    /// and every code path this enables is a complete no-op otherwise.
    pub fn set_kv_cache(
        &mut self,
        pool: Arc<KvSlotPool>,
        store: Arc<LlamaServerSlotStore>,
        backend_id: String,
        model_id: String,
        build_hash: String,
    ) {
        self.kv_cache = Some(KvCacheConfig { pool, store, backend_id, model_id, build_hash });
    }

    /// Checks out a slot (once per `Agent` lifetime -- idempotent, a
    /// no-op on the second and later calls within the same session) and
    /// either restores a previously-saved matching prefix into it, or
    /// warms it fresh with exactly this session's stable prefix (system
    /// prompt + tool defs + repo map -- via `system_prompt_text()`,
    /// never `self.history`) and saves it for future sessions. Every
    /// failure mode here is fail-open: logged at `warn`, `kv_slot_id`
    /// stays `None`, the turn proceeds exactly as if kvcache weren't
    /// configured at all.
    async fn ensure_kv_slot_checked_out(&mut self) {
        if self.kv_slot_id.is_some() {
            return; // already checked out earlier this session
        }
        let Some(kv) = &self.kv_cache else {
            return; // kvcache not configured for this Agent
        };
        let Some(slot_id) = kv.pool.checkout() else {
            tracing::warn!("kvcache: no free slot in the pool; this session runs unpinned");
            return;
        };

        let system_text = self.system_prompt_text();
        let tools = self.executor.definitions();
        let key = CacheKey {
            backend_id: kv.backend_id.clone(),
            model_id: kv.model_id.clone(),
            build_hash: kv.build_hash.clone(),
            prefix_hash: compute_prefix_hash(&system_text, &tools),
        };

        let hit = match kv.store.find(&key).await {
            Ok(handle) => handle.is_some(),
            Err(err) => {
                tracing::warn!(error = %err, "kvcache: find() failed; treating as a miss");
                false
            }
        };

        if hit {
            match kv.store.restore_into_slot(&key, slot_id).await {
                Ok(_) => {}
                Err(err) => tracing::warn!(error = %err, "kvcache: restore_into_slot failed"),
            }
        } else {
            // Cold: warm the slot with exactly the stable prefix, save it
            // once, then proceed. The warm-up goes through the *same*
            // `self.llm.stream_chat` path real turns use (not a raw
            // /completion call) so its tokenization matches exactly --
            // a mismatch here is what silently defeats automatic reuse.
            let warm_up_request = ChatRequest {
                messages: vec![Message::text(Role::System, system_text)],
                tools: Vec::new(),
                tool_choice: ToolChoice::Auto,
                temperature: None,
                max_tokens: Some(1),
                id_slot: Some(slot_id),
            };
            match self.llm.stream_chat(warm_up_request).await {
                Ok(mut stream) => {
                    use futures::StreamExt;
                    while stream.next().await.is_some() {
                        // Drain silently -- this call exists only to
                        // populate the slot, never shown to the user.
                    }
                    let meta = CacheMeta { size_bytes: 1, token_count: 1 };
                    if let Err(err) = kv.store.save_from_slot(&key, slot_id, meta).await {
                        tracing::warn!(error = %err, "kvcache: save_from_slot failed");
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "kvcache: warm-up request failed");
                }
            }
        }

        self.kv_slot_id = Some(slot_id);
    }
```

Call it from `run_turn_inner`, right alongside the other pre-loop refresh calls (`crates/aivyx-core/src/agent/mod.rs:1296-1298`):

```rust
        self.refresh_repo_map().await;
        self.refresh_agents_files(cwd).await;
        self.refresh_editor_context(cwd).await;
        self.ensure_kv_slot_checked_out().await;
```

Thread `id_slot` into the real `ChatRequest` built inside the iteration loop (`crates/aivyx-core/src/agent/mod.rs:1310`+, the existing `let request = ChatRequest { messages: ..., tools: ..., ... };` — add one field):

```rust
            let request = ChatRequest {
                messages: self.assemble_messages(),
                tools: {
                    // (unchanged -- existing tool-list logic stays exactly as it is)
                    let mut tools = if self.plan_mode.active() {
                        self.executor.plan_definitions()
                    } else {
                        self.executor.definitions()
                    };
                    if self.edit_format == EditFormat::Prompted {
                        tools.retain(|d| !PROMPTED_EDIT_HIDDEN_TOOLS.contains(&d.name.as_str()));
                    }
                    if self.autonomous_mode.active() {
                        tools.retain(|d| !AUTONOMOUS_HIDDEN_TOOLS.contains(&d.name.as_str()));
                    }
                    tools
                },
                id_slot: self.kv_slot_id,
                // (every other existing field stays exactly as it is)
```

(This last snippet shows only the fields this task touches — do not remove or reorder any of the surrounding, unshown fields already in that struct literal; add `id_slot: self.kv_slot_id,` as one more field in the existing literal.)

Add `impl Drop for Agent` (this crate currently has none — this is a new impl block, not a modification of an existing one):

```rust
impl Drop for Agent {
    /// Releases this session's checked-out kvcache slot, if any. Pure,
    /// synchronous, infallible -- `KvSlotPool::release` does no I/O, so
    /// this is safe to run from `Drop` (which cannot be async).
    fn drop(&mut self) {
        if let (Some(slot_id), Some(kv)) = (self.kv_slot_id, &self.kv_cache) {
            kv.pool.release(slot_id);
        }
    }
}
```

- [ ] **Step 8: Run the full test suite**

Run: `cargo test -p aivyx-core -- --test-threads=1`
Expected: PASS. Every pre-existing `Agent` test that constructs an `Agent` without calling `set_kv_cache` exercises `kv_cache: None`, so `ensure_kv_slot_checked_out` returns immediately and `ChatRequest.id_slot` stays `None` throughout — confirming this task adds no behavior change for any test (or config) that doesn't opt in.

- [ ] **Step 9: Commit**

```bash
git add crates/aivyx-core/src/agent/mod.rs
git commit -m "feat: checkout/warm-up/restore/pin/release kvcache policy in Agent"
```

---

### Task 6: Wire it up in `agent_builder.rs` and `aivyx-mcp-server`

**Files:**
- Modify: `crates/aivyx/src/agent_builder.rs` (real type: `pub(crate) struct BuiltAgent { agent: Agent, ..., llm: Arc<dyn LlmBackend>, confiner, checkpointer, repo_map, ... }`, `crates/aivyx/src/agent_builder.rs:43-62`)
- Modify: `crates/aivyx/src/main.rs` (real construction site: `aivyx_mcp_server::SessionConfig { llm: built.llm, confiner: built.confiner, checkpointer: built.checkpointer, repo_map: built.repo_map, base_registry: built.mcp_registry, deny_paths: built.deny_paths, cwd: built.cwd, context_tokens: ..., edit_format }`, `crates/aivyx/src/main.rs:222-232`)
- Modify: `crates/aivyx-mcp-server/src/session.rs` (real type: `pub struct SessionConfig { llm, confiner, checkpointer, repo_map, base_registry, deny_paths, cwd, context_tokens, edit_format }`, `crates/aivyx-mcp-server/src/session.rs:55-67`; real function: `pub async fn build_session_agent(config: &SessionConfig, level: AccessLevel, events_tx: ...) -> Agent`, `crates/aivyx-mcp-server/src/session.rs:99-104`)

**Interfaces:**
- Consumes: `Agent::set_kv_cache` (Task 5), `KvSlotPool::new` (Task 2), `aivyx_llm::probe::parse_llama_slots_info` (Task 4) fed the same `/props` JSON body `probe_served_context` already fetches (Step 1 fetches `/props` once and feeds it to both).
- Produces: `BuiltAgent.kv_cache_handles: Option<(Arc<KvSlotPool>, Arc<LlamaServerSlotStore>, String)>` (the `String` is `build_info`) flows from `agent_builder.rs` → `main.rs`'s `SessionConfig` construction → `aivyx-mcp-server`'s `SessionConfig` → `build_session_agent`, so the TUI/ACP agent and every MCP-server session share the *same* pool/store instances — critical for MCP-server mode, where `max_concurrent_sessions` (default 8) means multiple `Agent`s exist at once and must share one `total_slots`-sized pool, not get one each.

- [ ] **Step 1: Probe `/props` once, build the shared pool + store, in `agent_builder.rs`**

In `crates/aivyx/src/agent_builder.rs`, right after the existing `probe_served_context` call (`crates/aivyx/src/agent_builder.rs:514-523`), add:

```rust
    let kv_cache_handles = if settings.backend.kind == aivyx_config::BackendKind::LlamaServer {
        let origin = settings.backend.base_url.trim_end_matches('/').trim_end_matches("/v1");
        let props_url = format!("{origin}/props");
        match reqwest::Client::new().get(&props_url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
                Ok(json) => match aivyx_llm::probe::parse_llama_slots_info(&json) {
                    Some(info) => {
                        let store_path = dirs::data_local_dir()
                            .unwrap_or_else(std::env::temp_dir)
                            .join("aivyx-coder")
                            .join("kvcache");
                        match aivyx_kvcache::LlamaServerSlotStore::open(
                            &store_path,
                            &settings.backend.base_url,
                            10 * 1024 * 1024 * 1024, // 10 GiB default budget
                        ) {
                            Ok(store) => Some((
                                Arc::new(aivyx_llm::KvSlotPool::new(info.total_slots)),
                                Arc::new(store),
                                info.build_info,
                            )),
                            Err(err) => {
                                tracing::warn!(error = %err, "kvcache: failed to open store; disabled for this run");
                                None
                            }
                        }
                    }
                    None => {
                        tracing::warn!(
                            "kvcache: [backend] kind = \"llama_server\" but /props didn't look like a real \
                             llama-server response; disabled for this run"
                        );
                        None
                    }
                },
                Err(_) => None,
            },
            _ => {
                tracing::warn!("kvcache: /props probe failed; disabled for this run");
                None
            }
        }
    } else {
        None
    };
```

(`Arc` is already imported in this file — it constructs several other `Arc`-wrapped `BuiltAgent` fields, e.g. `Arc::clone(&llm)` at line 526.)

- [ ] **Step 2: Call `set_kv_cache` on the TUI/ACP agent, and add the new `BuiltAgent` field**

Right after the existing `let mut agent = Agent::new(...)` block (`crates/aivyx/src/agent_builder.rs:525`+), add:

```rust
    if let Some((pool, store, build_hash)) = &kv_cache_handles {
        agent.set_kv_cache(
            Arc::clone(pool),
            Arc::clone(store),
            "llama-server".to_string(),
            settings.backend.model.clone(),
            build_hash.clone(),
        );
    }
```

Add the new field to `BuiltAgent` (`crates/aivyx/src/agent_builder.rs:43-62`, alongside `repo_map`):

```rust
    pub(crate) kv_cache_handles: Option<(
        Arc<aivyx_llm::KvSlotPool>,
        Arc<aivyx_kvcache::LlamaServerSlotStore>,
        String,
    )>,
```

And set it in this function's final `BuiltAgent { ... }` struct literal (find it — the function's return value, alongside the existing `llm: Arc::clone(&llm), confiner, checkpointer, repo_map, ...` fields):

```rust
            kv_cache_handles,
```

- [ ] **Step 3: Thread it through `main.rs`'s `SessionConfig` construction**

In `crates/aivyx/src/main.rs`, add one field to the existing `aivyx_mcp_server::SessionConfig { ... }` literal (`crates/aivyx/src/main.rs:222-232`):

```rust
            session_config: aivyx_mcp_server::SessionConfig {
                llm: built.llm,
                confiner: built.confiner,
                checkpointer: built.checkpointer,
                repo_map: built.repo_map,
                kv_cache_handles: built.kv_cache_handles,
                base_registry: built.mcp_registry,
                deny_paths: built.deny_paths,
                cwd: built.cwd,
                context_tokens: settings.backend.context_tokens,
                edit_format,
            },
```

- [ ] **Step 4: Add the field to `aivyx-mcp-server`'s own `SessionConfig`, and call `set_kv_cache` in `build_session_agent`**

In `crates/aivyx-mcp-server/src/session.rs`, add to `SessionConfig` (`crates/aivyx-mcp-server/src/session.rs:55-67`, alongside `repo_map`):

```rust
    pub kv_cache_handles: Option<(
        Arc<aivyx_llm::KvSlotPool>,
        Arc<aivyx_kvcache::LlamaServerSlotStore>,
        String,
    )>,
```

In `build_session_agent` (`crates/aivyx-mcp-server/src/session.rs:99-104`), after the session's own `Agent` is constructed (find where this function builds and returns its `Agent` — add right before the final `agent` is returned):

```rust
    if let Some((pool, store, build_hash)) = &config.kv_cache_handles {
        agent.set_kv_cache(
            Arc::clone(pool),
            Arc::clone(store),
            "llama-server".to_string(),
            config.llm.model_id().to_string(),
            build_hash.clone(),
        );
    }
```

(`config.llm.model_id()` — `LlmBackend::model_id(&self) -> &str`, `crates/aivyx-llm/src/backend.rs:11` — is the real, already-available source of the configured model string here; `SessionConfig` has no separate model field of its own.)

- [ ] **Step 5: Add the new dependency to both crates' `Cargo.toml`**

`crates/aivyx/Cargo.toml` and `crates/aivyx-mcp-server/Cargo.toml` both need the same `aivyx-kvcache` dependency Task 5 added to `aivyx-core`'s `Cargo.toml`:

```toml
aivyx-kvcache = { git = "https://github.com/Aivyx-Agent/aivyx-kvcache", rev = "ee69d531c1e72dd6ea455c6554f37b010d69e981" }
```

- [ ] **Step 6: Build and run the full existing test suite**

Run: `cargo build --workspace && cargo test --workspace -- --test-threads=1`
Expected: builds cleanly, all existing tests pass (nothing here has automated coverage of its own yet beyond compiling correctly and not breaking anything — Task 7 is where this gets exercised for real).

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx/src/agent_builder.rs crates/aivyx/src/main.rs crates/aivyx/Cargo.toml crates/aivyx-mcp-server/src/session.rs crates/aivyx-mcp-server/Cargo.toml
git commit -m "feat: wire kvcache pool/store into agent_builder and MCP sessions"
```

---

### Task 7: Real end-to-end verification on the GPU rig

**Files:** none modified — this task verifies Tasks 1-6 against a real `llama-server` and produces a PASS/FAIL report. If it fails for a reason that points to a real bug in an earlier task's code, fix that task's file and re-run this task; don't mark it done on a failing run.

**Interfaces:**
- Consumes: everything from Tasks 1-6, plus the same rig (`10.80.80.148`) and Rust toolchain the `aivyx-kvcache` e2e-test project already installed there.

**Context for whoever runs this task:** this needs a real, running `llama-server` with a real GGUF model, matching the pattern already established for the `aivyx-kvcache` e2e test and this session's own earlier live-rig work. If you are a subagent without access to that real infrastructure, report `NEEDS_CONTEXT` rather than guessing.

- [ ] **Step 1: Build the `aivyx-coder` binary locally**

```bash
cargo build --release -p aivyx
```

- [ ] **Step 2: Copy it to the rig and configure it**

```bash
scp target/release/aivyx-coder 10.80.80.148:~/.local/bin/aivyx-coder
ssh 10.80.80.148 'chmod +x ~/.local/bin/aivyx-coder'
```

Ensure `~/.config/aivyx-coder/config.toml` on the rig has a `[backend]` section pointing at the rig's own llama-server with `kind = "llama_server"` (adjust `base_url`/`model`/`port` to match whatever `llama-server` invocation is currently running there — see the `aivyx-kvcache` e2e test's own plan, Task 2, for the exact rig conventions already established: model at `/home/julian/models/Qwen3.5-9B-Q4_K_M.gguf`, needs `--slot-save-path` set for this to do anything):

```toml
[backend]
base_url = "http://127.0.0.1:8080/v1"
model = "/home/julian/models/Qwen3.5-9B-Q4_K_M.gguf"
context_tokens = 16384
kind = "llama_server"
```

Start (or confirm already running) `llama-server` on the rig with `--slot-save-path` set (reuse the exact invocation from the `aivyx-kvcache` e2e test's own Task 2, Step 3 — `-ngl 99`, `--jinja`, the sampling flags, `--slot-save-path` pointed at a real directory, `--host 127.0.0.1 --port 8080`).

- [ ] **Step 3: Run one turn, restart the server, run an equivalent turn again**

```bash
ssh 10.80.80.148 'echo "list the files in the current directory" | ~/.local/bin/aivyx-coder 2>&1 | tail -20'
```

Expected: completes normally. Then check the kvcache store actually has a saved entry:

```bash
ssh 10.80.80.148 'find ~/.local/share/aivyx-coder/kvcache -type f 2>&1'
```

Expected: at least one `.slot` file and a `manifest.db` under that directory.

Restart `llama-server` (kill + respawn with the identical invocation, exactly like the `aivyx-kvcache` e2e project's Task 2 restart procedure — confirm via `pgrep`/`pkill` before respawning, watch for the same `pkill -f` self-match footgun that project's own report already documented: use `pkill -f "[l]lama-server"`, not `pkill -f llama-server`, when running the command itself contains that substring). Then run a second, equivalent turn (same repo/cwd, so the same stable prefix):

```bash
ssh 10.80.80.148 'echo "list the files in the current directory" | ~/.local/bin/aivyx-coder 2>&1 | tail -20'
```

Expected: completes normally, and — the actual thing this step verifies — the turn's own debug/trace output (or `AIVYX_DEBUG_LOG` if enabled, or the `tracing::warn!` absence in stderr) shows no `kvcache: ... failed` warnings, meaning the restore path succeeded rather than silently falling back to cold. If `AIVYX_DEBUG_LOG` is set for this run, its captured wire traffic will show `"id_slot"` present in the request body on this second run — the concrete, verifiable proof the pinning path fired for real, not just that the turn didn't error.

- [ ] **Step 4: Record the result**

No commit for this task. Report PASS with what was actually observed (the manifest/slot file contents, whether `id_slot` appeared in the real wire traffic on the second run, any warnings seen), or FAIL with the exact failure and which earlier task's code it points to.
