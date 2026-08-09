# `aivyx-recall` Cross-Session Memory Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `aivyx-coder` cross-session memory (`memory_write`/`memory_read`/`memory_forget`), backed by a new standalone crate/repo (`aivyx-recall`) that `aivyx` (the sibling personal-assistant repo) can later adopt too.

**Architecture:** A new, dependency-light repo `aivyx-recall` (sibling to this one, at `/home/julian/Projects/Rust/aivyx-recall`) ships a `Recall` trait plus two implementors: `InMemoryRecall` (test fake) and `FileRecall` (default persistent backend — one JSON file per topic, keyed the same way `aivyx-coder`'s own session files already are). `aivyx-coder` consumes it via a Cargo path dependency across the sibling directory and adds three new gated tools on top, plus a new `ActionKind::PersistentMemory` in `aivyx-sandbox` so writes/deletes get real confirmation gating (cacheable per topic, unconditionally denied under `--auto`) without reusing the existing, differently-shaped `ActionKind::Memory`.

**Tech Stack:** Rust, `async-trait`, `serde`/`serde_json`, `thiserror`, `tokio` (sync feature only, in `aivyx-recall`). No new dependency added to any *existing* crate in this workspace — `aivyx-tools` and `crates/aivyx` gain exactly one new dependency each (`aivyx-recall` itself, via path).

## Global Constraints

- Design source of truth: `docs/superpowers/specs/2026-08-09-aivyx-recall-design.md` (including its addendum on `--auto` handling and the relationship to `remember_preference`).
- `aivyx-recall` has **zero** dependency on `aivyx-storage`/`aivyx-crypto`/`aivyx-capability`/anything else from the `aivyx` repo, and zero dependency on any crate from this (`aivyx-coder`) workspace either — it is a fully standalone crate. Path dependencies point *into* it, never the reverse.
- `aivyx-tools` does not and must not depend on `aivyx-core` (dependency direction in this workspace is `aivyx-core → aivyx-tools`, confirmed in both crates' `Cargo.toml`). The cwd-hashing helper used for `project:`-scoped topics is therefore a small, deliberate, documented duplicate of `aivyx-core/src/session.rs`'s `fnv1a`, living in `aivyx-tools` instead — do not try to import or share it across that boundary.
- Every new persisted file (topic JSON files) is written `chmod 0600` on Unix, matching `session.rs::save`'s existing convention.
- `ActionKind::PersistentMemory` (new) is used by both `memory_write` and `memory_forget`. It is unconditionally denied under `--auto`, like `ActionKind::McpTool`/`ActionKind::Memory` — but, unlike `ActionKind::Memory`, it *does* participate in the Always-Allow cache (per exact topic string), so do not add it to `ConfirmationGate::check`'s `never_cached` condition.
- Run `cargo test --workspace -- --test-threads=1` in this repo (a known sandboxed-environment hang affects the default multi-threaded test runner here — always pass this flag). `aivyx-recall`'s own repo has no such history; plain `cargo test` there is fine.
- Run `cargo clippy --workspace --all-targets` clean (no *new* warnings) in both repos before the final task.

---

### Task 1: `aivyx-recall` repo scaffold + `Recall` trait + `InMemoryRecall`

**Files:**
- Create: `/home/julian/Projects/Rust/aivyx-recall/Cargo.toml`
- Create: `/home/julian/Projects/Rust/aivyx-recall/.gitignore`
- Create: `/home/julian/Projects/Rust/aivyx-recall/README.md`
- Create: `/home/julian/Projects/Rust/aivyx-recall/LICENSE-MIT`
- Create: `/home/julian/Projects/Rust/aivyx-recall/LICENSE-APACHE`
- Create: `/home/julian/Projects/Rust/aivyx-recall/src/lib.rs`
- Create: `/home/julian/Projects/Rust/aivyx-recall/src/in_memory.rs`
- Create: `/home/julian/Projects/Rust/aivyx-recall/src/conformance.rs`

**Interfaces:**
- Produces: `pub trait Recall: Send + Sync { async fn put(&self, topic: &str, body: &str) -> Result<u64, RecallError>; async fn get_recent(&self, topic: &str, limit: usize) -> Result<Vec<RecallEntry>, RecallError>; async fn forget(&self, topic: &str) -> Result<usize, RecallError>; }`, `pub struct RecallEntry { pub topic: String, pub body: String, pub seq: u64, pub created_at_secs: u64 }` (derives `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`), `pub enum RecallError { EmptyTopic, ZeroLimit, Encoding(String), Backend(String) }` (derives `Debug, Error`), `pub struct InMemoryRecall` with `pub fn new() -> Self`, and `pub(crate) async fn assert_conformance(recall: &dyn Recall)` (test-only, in `conformance.rs`) — all consumed by Task 2 (`FileRecall`) and every later task that constructs a `Recall`.

- [ ] **Step 1: Create the repo and its scaffolding files**

```bash
mkdir -p /home/julian/Projects/Rust/aivyx-recall/src
cd /home/julian/Projects/Rust/aivyx-recall
git init
cp /home/julian/Projects/Rust/aivyx-coder/LICENSE-MIT .
cp /home/julian/Projects/Rust/aivyx-coder/LICENSE-APACHE .
```

`Cargo.toml`:

```toml
[package]
name = "aivyx-recall"
description = "Storage-agnostic cross-session memory substrate for Aivyx agents"
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"

[dependencies]
async-trait = "0.1.89"
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.150"
thiserror = "2.0.18"
tokio = { version = "1.52.3", features = ["sync"] }

[dev-dependencies]
tempfile = "3.27.0"
tokio = { version = "1.52.3", features = ["rt", "macros"] }
```

`.gitignore`:

```
/target
```

`README.md`:

```markdown
# aivyx-recall

Storage-agnostic cross-session memory substrate for Aivyx agents.

A `Recall` trait (`put`/`get_recent`/`forget`, topic-scoped, sequence-ordered)
plus two implementations: `InMemoryRecall` (a deterministic fake, for
tests) and `FileRecall` (the default persistent backend — one JSON file
per topic).

Deliberately minimal — no embeddings, no ranking, no encryption, no
capability/audit model of its own. Consumers layer whatever scoping,
security, and retrieval semantics they need on top of the same trait.
Used by `aivyx-coder` (topic-scoped, on-demand recall tools) and,
eventually, `aivyx` (wrapping it with its own encrypted, capability-scoped
storage).

See `docs/superpowers/specs/2026-08-09-aivyx-recall-design.md` in the
`aivyx-coder` repo for the full design rationale.
```

- [ ] **Step 2: Write the failing conformance test, run against `InMemoryRecall`**

`src/lib.rs`:

```rust
//! Storage-agnostic cross-session memory substrate. See
//! `Recall`'s own doc comment for the contract every implementation
//! (this crate's own, or a consumer's) must satisfy.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod in_memory;
pub use in_memory::InMemoryRecall;

#[cfg(test)]
mod conformance;

/// A single stored memory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallEntry {
    /// Non-empty UTF-8 topic chosen by the caller. Entries with the same
    /// topic come back together, newest first, from `get_recent`.
    pub topic: String,
    /// UTF-8 body — whatever the caller asked to remember.
    pub body: String,
    /// Monotonic **per-topic** insertion counter, assigned by the
    /// substrate. First entry under a topic gets `seq = 0`, the next
    /// `seq = 1`, and so on — restarting at 0 if the topic is later
    /// `forget`-ten and written again. (This is a deliberate divergence
    /// from a single substrate-wide counter: it keeps a one-file-per-topic
    /// backend like `FileRecall` simple, since each topic's next `seq` is
    /// derivable from that topic's own file alone, with no shared
    /// cross-topic counter to persist or race.)
    pub seq: u64,
    /// Wall-clock seconds since UNIX epoch, captured by the substrate at
    /// write time.
    pub created_at_secs: u64,
}

/// Errors the memory substrate can return.
#[derive(Debug, Error)]
pub enum RecallError {
    #[error("recall topic must be non-empty")]
    EmptyTopic,
    #[error("recall get_recent limit must be > 0")]
    ZeroLimit,
    #[error("recall entry encoding error: {0}")]
    Encoding(String),
    #[error("recall backend error: {0}")]
    Backend(String),
}

/// The minimum memory substrate surface. Implementations are expected to
/// be called from multiple tasks concurrently (`Send + Sync`).
///
/// Contract every implementation must satisfy (exercised by
/// `conformance::assert_conformance`, run against every implementation in
/// this crate):
/// - `put`/`get_recent`/`forget` all fail fast with `EmptyTopic` on an
///   empty topic string.
/// - `get_recent` additionally fails with `ZeroLimit` when `limit == 0`.
/// - An unwritten topic's `get_recent` returns an empty `Vec`, not an
///   error.
/// - `get_recent` returns up to `limit` entries, newest first (`seq`
///   descending).
/// - `forget` deletes every entry under a topic and returns how many were
///   deleted; forgetting an already-empty topic returns `0`, not an
///   error.
/// - Topics are independent: writing to one never affects another.
#[async_trait]
pub trait Recall: Send + Sync {
    async fn put(&self, topic: &str, body: &str) -> Result<u64, RecallError>;
    async fn get_recent(&self, topic: &str, limit: usize) -> Result<Vec<RecallEntry>, RecallError>;
    async fn forget(&self, topic: &str) -> Result<usize, RecallError>;
}
```

`src/conformance.rs`:

```rust
//! Shared behavior contract, run against every `Recall` implementation in
//! this crate (`in_memory.rs`'s tests today; `file.rs`'s tests once Task 2
//! lands). See `Recall`'s own doc comment for what's being asserted here.

use crate::{Recall, RecallError};

pub(crate) async fn assert_conformance(recall: &dyn Recall) {
    // Empty-topic errors on all three methods.
    assert!(matches!(recall.put("", "x").await, Err(RecallError::EmptyTopic)));
    assert!(matches!(
        recall.get_recent("", 1).await,
        Err(RecallError::EmptyTopic)
    ));
    assert!(matches!(recall.forget("").await, Err(RecallError::EmptyTopic)));

    // Zero limit.
    assert!(matches!(
        recall.get_recent("t", 0).await,
        Err(RecallError::ZeroLimit)
    ));

    // Unwritten topic returns an empty Vec, not an error.
    assert_eq!(recall.get_recent("unwritten-topic", 10).await.unwrap(), vec![]);

    // seq is monotonic per topic; get_recent orders newest first.
    let seq0 = recall.put("topic-a", "first").await.unwrap();
    let seq1 = recall.put("topic-a", "second").await.unwrap();
    assert_eq!(seq1, seq0 + 1);
    let recent = recall.get_recent("topic-a", 10).await.unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].body, "second");
    assert_eq!(recent[0].seq, seq1);
    assert_eq!(recent[1].body, "first");
    assert_eq!(recent[1].seq, seq0);

    // limit caps results, keeping the newest.
    let limited = recall.get_recent("topic-a", 1).await.unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].body, "second");

    // forget returns the count deleted and clears the topic.
    let deleted = recall.forget("topic-a").await.unwrap();
    assert_eq!(deleted, 2);
    assert_eq!(recall.get_recent("topic-a", 10).await.unwrap(), vec![]);

    // Forgetting an already-empty topic is a no-op, not an error.
    assert_eq!(recall.forget("topic-a").await.unwrap(), 0);

    // Topics are independent.
    recall.put("topic-b", "b-entry").await.unwrap();
    assert_eq!(recall.get_recent("topic-a", 10).await.unwrap(), vec![]);
    assert_eq!(recall.get_recent("topic-b", 10).await.unwrap().len(), 1);
}
```

`src/in_memory.rs` (test module only for this step — the real impl is Step 3):

```rust
use crate::conformance::assert_conformance;

pub struct InMemoryRecall;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn satisfies_the_recall_contract() {
        assert_conformance(&InMemoryRecall).await;
    }
}
```

Run: `cd /home/julian/Projects/Rust/aivyx-recall && cargo test`
Expected: FAIL to compile — `InMemoryRecall` doesn't implement `Recall` yet.

- [ ] **Step 2: Implement `InMemoryRecall`**

Replace `src/in_memory.rs` with:

```rust
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::{Recall, RecallEntry, RecallError};

/// Deterministic in-process fake, no persistence. For tests in this crate
/// and in consumers that don't want real filesystem I/O.
#[derive(Default)]
pub struct InMemoryRecall {
    entries: Mutex<HashMap<String, Vec<RecallEntry>>>,
}

impl InMemoryRecall {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Recall for InMemoryRecall {
    async fn put(&self, topic: &str, body: &str) -> Result<u64, RecallError> {
        if topic.is_empty() {
            return Err(RecallError::EmptyTopic);
        }
        let mut map = self.entries.lock().unwrap();
        let list = map.entry(topic.to_string()).or_default();
        let seq = list.iter().map(|e| e.seq).max().map(|m| m + 1).unwrap_or(0);
        let created_at_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        list.push(RecallEntry {
            topic: topic.to_string(),
            body: body.to_string(),
            seq,
            created_at_secs,
        });
        Ok(seq)
    }

    async fn get_recent(&self, topic: &str, limit: usize) -> Result<Vec<RecallEntry>, RecallError> {
        if topic.is_empty() {
            return Err(RecallError::EmptyTopic);
        }
        if limit == 0 {
            return Err(RecallError::ZeroLimit);
        }
        let map = self.entries.lock().unwrap();
        let mut matching: Vec<RecallEntry> = map.get(topic).cloned().unwrap_or_default();
        matching.sort_by(|a, b| b.seq.cmp(&a.seq));
        matching.truncate(limit);
        Ok(matching)
    }

    async fn forget(&self, topic: &str) -> Result<usize, RecallError> {
        if topic.is_empty() {
            return Err(RecallError::EmptyTopic);
        }
        Ok(self
            .entries
            .lock()
            .unwrap()
            .remove(topic)
            .map(|v| v.len())
            .unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conformance::assert_conformance;

    #[tokio::test]
    async fn satisfies_the_recall_contract() {
        assert_conformance(&InMemoryRecall::new()).await;
    }
}
```

Run: `cargo test`
Expected: PASS (1 test).

- [ ] **Step 3: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-recall
git add -A
git commit -m "Add Recall trait, RecallEntry/RecallError, and InMemoryRecall

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 2: `FileRecall` persistent backend

**Files:**
- Create: `/home/julian/Projects/Rust/aivyx-recall/src/file.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-recall/src/lib.rs`

**Interfaces:**
- Consumes: `Recall`, `RecallEntry`, `RecallError` (Task 1), `conformance::assert_conformance` (Task 1, test-only).
- Produces: `pub struct FileRecall` with `pub fn new(dir: impl Into<PathBuf>) -> Self` — consumed by Task 9 (`agent_builder.rs`).

- [ ] **Step 1: Write the failing tests**

`src/file.rs`:

```rust
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{Recall, RecallEntry, RecallError};

/// The default persistent `Recall` backend: one JSON file per topic under
/// `dir`. Filename = a sanitized topic prefix + an FNV-1a hash of the full
/// topic string — the same scheme `aivyx-coder`'s own session-persistence
/// layer uses for its session keys (inlined FNV-1a, not `std`'s
/// `DefaultHasher`, which doesn't guarantee stability across program
/// versions; the filename must stay stable across restarts and upgrades).
/// Written via `std::fs::write` then `chmod 0600` on Unix — direct write,
/// not atomic-via-tempfile-rename, matching that same layer's accepted
/// best-effort tradeoff (a torn write from a mid-write crash loses that
/// one topic's file, not more).
///
/// A single `tokio::sync::Mutex` serializes every read-modify-write across
/// every topic — simple and correct at personal-memory scale; no
/// per-topic lock bookkeeping.
pub struct FileRecall {
    dir: PathBuf,
    lock: Mutex<()>,
}

#[derive(Serialize, Deserialize, Default)]
struct TopicFile {
    entries: Vec<RecallEntry>,
}

impl FileRecall {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            lock: Mutex::new(()),
        }
    }

    fn topic_path(&self, topic: &str) -> PathBuf {
        let sanitized: String = topic
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .take(40)
            .collect();
        self.dir
            .join(format!("{sanitized}-{:016x}.json", fnv1a(topic.as_bytes())))
    }

    fn load(&self, topic: &str) -> Result<Vec<RecallEntry>, RecallError> {
        let path = self.topic_path(topic);
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                let file: TopicFile =
                    serde_json::from_str(&raw).map_err(|e| RecallError::Encoding(e.to_string()))?;
                Ok(file.entries)
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(Vec::new()),
            Err(err) => Err(RecallError::Backend(err.to_string())),
        }
    }

    fn save(&self, topic: &str, entries: &[RecallEntry]) -> Result<(), RecallError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| RecallError::Backend(e.to_string()))?;
        let path = self.topic_path(topic);
        let json = serde_json::to_string_pretty(&TopicFile {
            entries: entries.to_vec(),
        })
        .map_err(|e| RecallError::Encoding(e.to_string()))?;
        std::fs::write(&path, json).map_err(|e| RecallError::Backend(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

#[async_trait]
impl Recall for FileRecall {
    async fn put(&self, topic: &str, body: &str) -> Result<u64, RecallError> {
        if topic.is_empty() {
            return Err(RecallError::EmptyTopic);
        }
        let _guard = self.lock.lock().await;
        let mut entries = self.load(topic)?;
        let seq = entries.iter().map(|e| e.seq).max().map(|m| m + 1).unwrap_or(0);
        let created_at_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        entries.push(RecallEntry {
            topic: topic.to_string(),
            body: body.to_string(),
            seq,
            created_at_secs,
        });
        self.save(topic, &entries)?;
        Ok(seq)
    }

    async fn get_recent(&self, topic: &str, limit: usize) -> Result<Vec<RecallEntry>, RecallError> {
        if topic.is_empty() {
            return Err(RecallError::EmptyTopic);
        }
        if limit == 0 {
            return Err(RecallError::ZeroLimit);
        }
        let _guard = self.lock.lock().await;
        let mut entries = self.load(topic)?;
        entries.sort_by(|a, b| b.seq.cmp(&a.seq));
        entries.truncate(limit);
        Ok(entries)
    }

    async fn forget(&self, topic: &str) -> Result<usize, RecallError> {
        if topic.is_empty() {
            return Err(RecallError::EmptyTopic);
        }
        let _guard = self.lock.lock().await;
        let entries = self.load(topic)?;
        let count = entries.len();
        if count > 0 {
            std::fs::remove_file(self.topic_path(topic))
                .map_err(|e| RecallError::Backend(e.to_string()))?;
        }
        Ok(count)
    }
}

/// Must stay byte-identical to `aivyx-coder`'s `crates/aivyx-core/src/session.rs::fnv1a` —
/// see that function's own doc comment for why `std::collections::hash_map::DefaultHasher`
/// isn't used here.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conformance::assert_conformance;

    #[tokio::test]
    async fn satisfies_the_recall_contract() {
        let dir = tempfile::tempdir().unwrap();
        let recall = FileRecall::new(dir.path());
        assert_conformance(&recall).await;
    }

    #[tokio::test]
    async fn entries_persist_across_a_fresh_instance_pointed_at_the_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        {
            let recall = FileRecall::new(dir.path());
            recall.put("topic", "remember this").await.unwrap();
        }
        let reopened = FileRecall::new(dir.path());
        let entries = reopened.get_recent("topic", 10).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].body, "remember this");
    }

    #[test]
    #[cfg(unix)]
    fn topic_file_is_written_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let recall = FileRecall::new(dir.path());
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(recall.put("topic", "secret"))
            .unwrap();
        let path = recall.topic_path("topic");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
```

Add to `src/lib.rs` (after the existing `mod in_memory;` line):

```rust
mod file;
pub use file::FileRecall;
```

Run: `cd /home/julian/Projects/Rust/aivyx-recall && cargo test`
Expected: FAIL — new tests don't compile/pass yet if any typo exists; more likely this compiles clean on the first try since the implementation is written in the same step as the test per this crate's small size, but run it anyway to confirm before moving on. If it fails for a real reason, fix `file.rs` until green.

- [ ] **Step 2: Run tests, confirm green**

Run: `cargo test`
Expected: PASS (4 new tests: `satisfies_the_recall_contract`, `entries_persist_across_a_fresh_instance_pointed_at_the_same_dir`, `topic_file_is_written_owner_only`, plus `in_memory.rs`'s existing test).

- [ ] **Step 3: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-recall
git add -A
git commit -m "Add FileRecall, the default persistent Recall backend

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 3: `ActionKind::PersistentMemory` in `aivyx-sandbox`

**Files:**
- Modify: `crates/aivyx-sandbox/src/lib.rs`
- Modify: `crates/aivyx-sandbox/src/confirmation.rs`

**Interfaces:**
- Produces: `ActionKind::PersistentMemory` variant — consumed by Task 6 (`MemoryWriteTool`) and Task 7 (`MemoryForgetTool`).

- [ ] **Step 1: Write the failing test**

Add to `crates/aivyx-sandbox/src/confirmation.rs`'s `#[cfg(test)] mod tests` block (near `memory_request`/`autonomous_mode_denies_memory_actions_unconditionally`, around line 1697):

```rust
    fn persistent_memory_request(topic: &str) -> PermissionRequest {
        PermissionRequest {
            tool_name: "memory_write".to_string(),
            action: ActionKind::PersistentMemory,
            target: PermissionTarget::Other(topic.to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        }
    }

    #[tokio::test]
    async fn autonomous_mode_denies_persistent_memory_actions_unconditionally() {
        // Mirrors autonomous_mode_denies_memory_actions_unconditionally
        // exactly, for the new ActionKind — memory_write/memory_forget
        // persist outside the project with no checkpoint/rollback net,
        // same reasoning as remember_preference's ActionKind::Memory.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate.check(&persistent_memory_request("project:flaky-tests")).await;

        assert!(matches!(decision, PermissionDecision::Deny(_)));
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            0,
            "autonomous mode must never prompt for a PersistentMemory action"
        );
    }

    #[tokio::test]
    async fn persistent_memory_actions_are_cached_unlike_plain_memory_actions() {
        // The key difference from ActionKind::Memory: here the target
        // (topic string) genuinely varies per call, so per-topic
        // Always-Allow caching is safe and intended.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::AllowAlways,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let first = gate.check(&persistent_memory_request("project:flaky-tests")).await;
        assert_eq!(first, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

        let second = gate.check(&persistent_memory_request("project:flaky-tests")).await;
        assert_eq!(second, PermissionDecision::AllowAlways);
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            1,
            "same exact topic should be cached, unlike ActionKind::Memory"
        );

        let third = gate.check(&persistent_memory_request("project:other-topic")).await;
        assert_eq!(third, PermissionDecision::AllowAlways);
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            2,
            "a different topic must still prompt"
        );
    }
```

Run: `cargo test -p aivyx-sandbox -- --test-threads=1`
Expected: FAIL to compile — `ActionKind::PersistentMemory` doesn't exist yet.

- [ ] **Step 2: Add the `ActionKind` variant**

In `crates/aivyx-sandbox/src/lib.rs`, add after the `Move` variant (end of the `ActionKind` enum, before its closing `}`):

```rust
    /// The agent persisting a fact under a topic (`memory_write`) or
    /// deleting one (`memory_forget`) — see `aivyx-tools`'
    /// `memory_write`/`memory_forget`. Unlike `Memory` (used only by
    /// `remember_preference`), the target here is the actual topic
    /// string, which genuinely varies per call — so, unlike `Memory`,
    /// this kind *does* participate in the Always-Allow cache, keyed on
    /// that exact topic. It shares `Memory`'s unconditional `--auto`
    /// denial, though: both persist content outside the project working
    /// tree, with no git checkpoint/rollback safety net to fall back on
    /// if an unattended run gets it wrong.
    PersistentMemory,
```

Run: `cargo build -p aivyx-sandbox`
Expected: compiles (the enum itself has no exhaustive-match sites yet that would break — `PermissionKey::from_request` matches on `PermissionTarget`, not `ActionKind`, so it's unaffected).

- [ ] **Step 3: Wire the autonomous-mode denial**

In `crates/aivyx-sandbox/src/confirmation.rs`, add a new constant near `AUTONOMOUS_MEMORY_DENIAL`:

```rust
/// Told to the model when a `memory_write`/`memory_forget` call reaches
/// autonomous mode. Same reasoning as `AUTONOMOUS_MEMORY_DENIAL`: the
/// change persists outside the project working tree, with no
/// checkpoint/rollback safety net, and there is no human present to
/// review it.
const AUTONOMOUS_PERSISTENT_MEMORY_DENIAL: &str =
    "remembering or forgetting persistent memory requires interactive confirmation and cannot \
     happen in autonomous mode";
```

Then, in `ConfirmationGate::check`'s autonomous-mode branch, add a new arm immediately after the existing `if request.action == ActionKind::Memory { ... }` block:

```rust
            if request.action == ActionKind::PersistentMemory {
                tracing::warn!(
                    tool = %request.tool_name,
                    action = ?request.action,
                    target = ?request.target,
                    "permission denied: memory_write/memory_forget call in autonomous mode"
                );
                return PermissionDecision::Deny(Some(
                    AUTONOMOUS_PERSISTENT_MEMORY_DENIAL.to_string(),
                ));
            }
```

Do **not** touch the `never_cached` line (`let never_cached = request.action == ActionKind::Memory;`) — `PersistentMemory` must stay out of it so the second test above passes.

Run: `cargo test -p aivyx-sandbox -- --test-threads=1`
Expected: PASS, including the two new tests.

- [ ] **Step 4: Run the full `aivyx-sandbox` suite and commit**

Run: `cargo test -p aivyx-sandbox -- --test-threads=1`
Expected: PASS, no regressions in the rest of the crate's existing tests.

```bash
git add crates/aivyx-sandbox/src/lib.rs crates/aivyx-sandbox/src/confirmation.rs
git commit -m "Add ActionKind::PersistentMemory: cacheable, still --auto-denied

Backs the upcoming memory_write/memory_forget tools. Unlike the existing
ActionKind::Memory (remember_preference's fixed-target, never-cached
kind), this one supports normal per-topic Always-Allow caching — but
shares Memory's unconditional autonomous-mode denial, since both persist
content outside the project working tree with no checkpoint/rollback
safety net.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 4: `memory_topic` shared module in `aivyx-tools`

**Files:**
- Create: `crates/aivyx-tools/src/memory_topic.rs`
- Modify: `crates/aivyx-tools/src/lib.rs`

**Interfaces:**
- Produces: `pub(crate) fn resolve_topic(cwd: &Path, raw_topic: &str) -> Result<String, ToolError>` — consumed by Task 5 (`MemoryReadTool`), Task 6 (`MemoryWriteTool`), Task 7 (`MemoryForgetTool`).

- [ ] **Step 1: Write the failing tests**

`crates/aivyx-tools/src/memory_topic.rs`:

```rust
//! Topic-prefix validation and cwd-scoped key rewriting shared by
//! `memory_write`/`memory_read`/`memory_forget`.
//!
//! Kept out of `aivyx-core` (which computes an identical cwd hash for
//! session keys in `session.rs`) because `aivyx-tools` does not depend on
//! `aivyx-core` — the dependency direction in this workspace runs the
//! other way. This ~10-line FNV-1a is a deliberate, justified duplicate
//! of `session.rs`'s own, for the same reason `deny_paths` matching was
//! once duplicated between `aivyx-sandbox` and `aivyx-repomap`: the crate
//! boundary matters more than avoiding a small, stable, well-tested
//! duplicate. Must stay byte-identical to `session.rs::fnv1a` — if either
//! changes, update both.

use std::path::Path;

use crate::ToolError;

const GLOBAL_PREFIX: &str = "global:";
const PROJECT_PREFIX: &str = "project:";

/// Rewrites a model-supplied topic into its internal storage key.
/// `project:<rest>` becomes `project:<cwd-hash>:<rest>` so the same
/// nominal topic in two different project directories never collides;
/// `global:<rest>` passes through unchanged. Any other prefix (or no
/// prefix) is rejected so the model can't silently write into an
/// unnamespaced key.
pub(crate) fn resolve_topic(cwd: &Path, raw_topic: &str) -> Result<String, ToolError> {
    if let Some(rest) = raw_topic.strip_prefix(GLOBAL_PREFIX) {
        if rest.is_empty() {
            return Err(ToolError::InvalidArguments(
                "topic must have content after \"global:\"".to_string(),
            ));
        }
        return Ok(raw_topic.to_string());
    }
    if let Some(rest) = raw_topic.strip_prefix(PROJECT_PREFIX) {
        if rest.is_empty() {
            return Err(ToolError::InvalidArguments(
                "topic must have content after \"project:\"".to_string(),
            ));
        }
        let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        return Ok(format!(
            "project:{:016x}:{rest}",
            fnv1a(canonical.to_string_lossy().as_bytes())
        ));
    }
    Err(ToolError::InvalidArguments(format!(
        "topic must start with \"global:\" or \"project:\" (got {raw_topic:?})"
    )))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_prefix_passes_through_unchanged() {
        let resolved = resolve_topic(Path::new("/irrelevant"), "global:editor").unwrap();
        assert_eq!(resolved, "global:editor");
    }

    #[test]
    fn project_prefix_gets_a_cwd_hash_inserted() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_topic(dir.path(), "project:flaky-tests").unwrap();
        assert!(resolved.starts_with("project:"));
        assert!(resolved.ends_with(":flaky-tests"));
        assert_ne!(resolved, "project:flaky-tests");
    }

    #[test]
    fn same_project_topic_resolves_identically_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let first = resolve_topic(dir.path(), "project:flaky-tests").unwrap();
        let second = resolve_topic(dir.path(), "project:flaky-tests").unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn different_project_dirs_resolve_the_same_topic_differently() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let a = resolve_topic(dir_a.path(), "project:flaky-tests").unwrap();
        let b = resolve_topic(dir_b.path(), "project:flaky-tests").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn bare_topic_is_rejected() {
        let err = resolve_topic(Path::new("/irrelevant"), "flaky-tests").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn empty_prefix_content_is_rejected() {
        let err = resolve_topic(Path::new("/irrelevant"), "global:").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
```

Add to `crates/aivyx-tools/src/lib.rs` (alphabetically among the existing `mod path_resolve; mod diff;`-style declarations near the top):

```rust
mod memory_topic;
```

Run: `cargo test -p aivyx-tools memory_topic -- --test-threads=1`
Expected: PASS immediately (the implementation is written in the same step as the tests, matching this crate's other small-module precedent) — run it to confirm rather than assuming.

- [ ] **Step 2: Commit**

```bash
git add crates/aivyx-tools/src/memory_topic.rs crates/aivyx-tools/src/lib.rs
git commit -m "Add memory_topic: global:/project: prefix validation and cwd-scoped rewriting

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 5: `MemoryReadTool`

**Files:**
- Create: `crates/aivyx-tools/src/tools/memory_read.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs`
- Modify: `crates/aivyx-tools/Cargo.toml`

**Interfaces:**
- Consumes: `aivyx_recall::Recall` (Task 1/2, from the `aivyx-recall` crate — this task adds the path dependency), `memory_topic::resolve_topic` (Task 4).
- Produces: `pub struct MemoryReadTool` with `pub fn new(recall: Arc<dyn Recall>) -> Self` — consumed by Task 9 (`agent_builder.rs`).

- [ ] **Step 1: Add the `aivyx-recall` dependency**

In `crates/aivyx-tools/Cargo.toml`, add under `[dependencies]` (first entry, alphabetically before `aivyx-sandbox`):

```toml
aivyx-recall = { path = "../../../aivyx-recall" }
```

Run: `cargo build -p aivyx-tools`
Expected: succeeds (the path now resolves to the repo Task 1 created).

- [ ] **Step 2: Write the failing tests**

`crates/aivyx-tools/src/tools/memory_read.rs`:

```rust
use std::path::Path;
use std::sync::Arc;

use aivyx_recall::Recall;
use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::memory_topic::resolve_topic;
use crate::{Tool, ToolError, ToolExecutionContext};

fn default_limit() -> usize {
    10
}

#[derive(Deserialize, JsonSchema)]
struct MemoryReadArgs {
    /// Exact topic to recall, e.g. "project:flaky-tests" or
    /// "global:editor-preference". Must start with "global:" or
    /// "project:".
    topic: String,
    /// Maximum number of entries to return, newest first.
    #[serde(default = "default_limit")]
    limit: usize,
}

pub struct MemoryReadTool {
    recall: Arc<dyn Recall>,
}

impl MemoryReadTool {
    pub fn new(recall: Arc<dyn Recall>) -> Self {
        Self { recall }
    }
}

#[async_trait]
impl Tool for MemoryReadTool {
    fn name(&self) -> &str {
        "memory_read"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Recall entries previously saved with memory_write under an exact \
                topic, newest first. Returns an empty list if nothing has been saved under that \
                topic. Topic must start with \"global:\" (across every project) or \"project:\" \
                (this project only)."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(MemoryReadArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: MemoryReadArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve_topic(cwd, &args.topic)?;

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Other(resolved),
            arguments_preview: json!({ "topic": args.topic, "limit": args.limit }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: MemoryReadArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        if args.limit == 0 {
            return Err(ToolError::InvalidArguments("limit must be > 0".to_string()));
        }
        let resolved = resolve_topic(&ctx.cwd, &args.topic)?;

        let entries = self
            .recall
            .get_recent(&resolved, args.limit)
            .await
            .map_err(|err| ToolError::ExecutionFailed(err.to_string()))?;

        if entries.is_empty() {
            return Ok(ToolOutput::Ok(format!("no memory saved under {:?}", args.topic)));
        }

        let rendered = entries
            .iter()
            .map(|e| format!("- {}", e.body))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::Ok(rendered))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // No shared ToolExecutionContext test helper exists in this crate —
    // every tool's own test module defines a local one. Mirrors
    // remember_preference.rs's `fn ctx(dir: &Path)` exactly.
    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[test]
    fn permission_request_targets_read_action_and_resolved_topic() {
        let tool = MemoryReadTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let request = tool
            .permission_request(&json!({"topic": "global:editor"}), Path::new("/irrelevant"))
            .unwrap();

        assert_eq!(request.action, ActionKind::Read);
        assert_eq!(request.target, PermissionTarget::Other("global:editor".to_string()));
    }

    #[test]
    fn permission_request_rejects_a_bare_topic() {
        let tool = MemoryReadTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let err = tool
            .permission_request(&json!({"topic": "editor"}), Path::new("/irrelevant"))
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn execute_returns_a_placeholder_message_for_an_unwritten_topic() {
        let tool = MemoryReadTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let dir = tempfile::tempdir().unwrap();

        let output = tool
            .execute(json!({"topic": "global:never-written"}), &ctx(dir.path()))
            .await
            .unwrap();

        match output {
            ToolOutput::Ok(text) => assert!(text.contains("no memory saved")),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_returns_previously_written_entries_newest_first() {
        let recall = Arc::new(aivyx_recall::InMemoryRecall::new());
        recall.put("global:notes", "first").await.unwrap();
        recall.put("global:notes", "second").await.unwrap();
        let tool = MemoryReadTool::new(recall);
        let dir = tempfile::tempdir().unwrap();

        let output = tool
            .execute(json!({"topic": "global:notes"}), &ctx(dir.path()))
            .await
            .unwrap();

        match output {
            ToolOutput::Ok(text) => {
                let second_pos = text.find("second").expect("should contain 'second'");
                let first_pos = text.find("first").expect("should contain 'first'");
                assert!(second_pos < first_pos, "newest entry should come first");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }
}
```

In `crates/aivyx-tools/src/tools/mod.rs`, insert `mod memory_read;` between the existing `mod mcp_tool;` and `mod move_file;` lines, and `pub use memory_read::MemoryReadTool;` at the matching position in the `pub use` block below (also between the `mcp_tool`/`move_file` lines):

```rust
mod memory_read;
```
```rust
pub use memory_read::MemoryReadTool;
```

Run: `cargo test -p aivyx-tools memory_read -- --test-threads=1`
Expected: FAIL to compile initially only if there's a typo; this tool's implementation is written in the same step as its tests (matching this crate's small-tool precedent) — run to confirm PASS.

- [ ] **Step 3: Run and confirm green, then commit**

Run: `cargo test -p aivyx-tools memory_read -- --test-threads=1`
Expected: PASS (4 tests).

```bash
git add crates/aivyx-tools/Cargo.toml crates/aivyx-tools/src/tools/memory_read.rs crates/aivyx-tools/src/tools/mod.rs
git commit -m "Add memory_read tool

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 6: `MemoryWriteTool`

**Files:**
- Create: `crates/aivyx-tools/src/tools/memory_write.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs`

**Interfaces:**
- Consumes: `aivyx_recall::Recall` (Task 1/2), `memory_topic::resolve_topic` (Task 4), `ActionKind::PersistentMemory` (Task 3).
- Produces: `pub struct MemoryWriteTool` with `pub fn new(recall: Arc<dyn Recall>) -> Self` — consumed by Task 9.

- [ ] **Step 1: Write the failing tests**

`crates/aivyx-tools/src/tools/memory_write.rs`:

```rust
use std::path::Path;
use std::sync::Arc;

use aivyx_recall::Recall;
use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::memory_topic::resolve_topic;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct MemoryWriteArgs {
    /// Topic to file this under, e.g. "project:flaky-tests" or
    /// "global:editor-preference". Must start with "global:" or
    /// "project:".
    topic: String,
    /// The fact or note to remember.
    body: String,
}

pub struct MemoryWriteTool {
    recall: Arc<dyn Recall>,
}

impl MemoryWriteTool {
    pub fn new(recall: Arc<dyn Recall>) -> Self {
        Self { recall }
    }
}

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &str {
        "memory_write"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Persist a small fact or note for future recall via memory_read — \
                not shown to you automatically. Use \"project:<name>\" for something specific \
                to this project (e.g. \"project:flaky-tests\"), or \"global:<name>\" for \
                something true across every project (e.g. \"global:editor-preference\"). For \
                standing instructions that should always be active, use remember_preference \
                instead."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(MemoryWriteArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: MemoryWriteArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve_topic(cwd, &args.topic)?;

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::PersistentMemory,
            target: PermissionTarget::Other(resolved),
            arguments_preview: json!({ "topic": args.topic }),
            preview: Some(args.body.clone()),
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: MemoryWriteArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve_topic(&ctx.cwd, &args.topic)?;

        self.recall
            .put(&resolved, &args.body)
            .await
            .map_err(|err| ToolError::ExecutionFailed(err.to_string()))?;

        Ok(ToolOutput::Ok(format!("remembered under {:?}", args.topic)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[test]
    fn permission_request_uses_persistent_memory_action_and_resolved_topic() {
        let tool = MemoryWriteTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let request = tool
            .permission_request(
                &json!({"topic": "global:editor", "body": "prefers tabs"}),
                Path::new("/irrelevant"),
            )
            .unwrap();

        assert_eq!(request.action, ActionKind::PersistentMemory);
        assert_eq!(request.target, PermissionTarget::Other("global:editor".to_string()));
        assert_eq!(request.preview, Some("prefers tabs".to_string()));
    }

    #[test]
    fn permission_request_rejects_a_bare_topic() {
        let tool = MemoryWriteTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let err = tool
            .permission_request(
                &json!({"topic": "editor", "body": "x"}),
                Path::new("/irrelevant"),
            )
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn execute_persists_the_entry_so_it_can_be_read_back() {
        let recall = Arc::new(aivyx_recall::InMemoryRecall::new());
        let tool = MemoryWriteTool::new(Arc::clone(&recall) as Arc<dyn Recall>);
        let dir = tempfile::tempdir().unwrap();

        tool.execute(
            json!({"topic": "global:editor", "body": "prefers tabs"}),
            &ctx(dir.path()),
        )
        .await
        .unwrap();

        let entries = recall.get_recent("global:editor", 10).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].body, "prefers tabs");
    }

    #[tokio::test]
    async fn execute_scopes_a_project_topic_under_the_cwd_hash() {
        let recall = Arc::new(aivyx_recall::InMemoryRecall::new());
        let tool = MemoryWriteTool::new(Arc::clone(&recall) as Arc<dyn Recall>);
        let dir = tempfile::tempdir().unwrap();

        tool.execute(
            json!({"topic": "project:flaky-tests", "body": "cargo test -p foo is flaky"}),
            &ctx(dir.path()),
        )
        .await
        .unwrap();

        // The literal, unscoped topic was never written.
        assert_eq!(
            recall.get_recent("project:flaky-tests", 10).await.unwrap(),
            vec![]
        );
    }
}
```

In `crates/aivyx-tools/src/tools/mod.rs`, insert `mod memory_write;` between the `memory_read`/`move_file` lines (both blocks — `mod` and `pub use`):

```rust
mod memory_write;
```
```rust
pub use memory_write::MemoryWriteTool;
```

Run: `cargo test -p aivyx-tools memory_write -- --test-threads=1`
Expected: PASS (4 tests).

- [ ] **Step 2: Commit**

```bash
git add crates/aivyx-tools/src/tools/memory_write.rs crates/aivyx-tools/src/tools/mod.rs
git commit -m "Add memory_write tool

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 7: `MemoryForgetTool`

**Files:**
- Create: `crates/aivyx-tools/src/tools/memory_forget.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs`

**Interfaces:**
- Consumes: `aivyx_recall::Recall` (Task 1/2), `memory_topic::resolve_topic` (Task 4), `ActionKind::PersistentMemory` (Task 3).
- Produces: `pub struct MemoryForgetTool` with `pub fn new(recall: Arc<dyn Recall>) -> Self` — consumed by Task 9.

- [ ] **Step 1: Write the failing tests**

`crates/aivyx-tools/src/tools/memory_forget.rs`:

```rust
use std::path::Path;
use std::sync::Arc;

use aivyx_recall::Recall;
use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::memory_topic::resolve_topic;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct MemoryForgetArgs {
    /// Exact topic to forget entirely, e.g. "project:flaky-tests". Must
    /// start with "global:" or "project:".
    topic: String,
}

pub struct MemoryForgetTool {
    recall: Arc<dyn Recall>,
}

impl MemoryForgetTool {
    pub fn new(recall: Arc<dyn Recall>) -> Self {
        Self { recall }
    }
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn name(&self) -> &str {
        "memory_forget"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Permanently delete every entry saved with memory_write under an \
                exact topic. Returns how many entries were deleted (0 if the topic was never \
                written). Topic must start with \"global:\" or \"project:\"."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(MemoryForgetArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: MemoryForgetArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve_topic(cwd, &args.topic)?;

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::PersistentMemory,
            target: PermissionTarget::Other(resolved),
            arguments_preview: json!({ "topic": args.topic }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: MemoryForgetArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve_topic(&ctx.cwd, &args.topic)?;

        let count = self
            .recall
            .forget(&resolved)
            .await
            .map_err(|err| ToolError::ExecutionFailed(err.to_string()))?;

        Ok(ToolOutput::Ok(format!(
            "deleted {count} entries under {:?}",
            args.topic
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[test]
    fn permission_request_uses_persistent_memory_action_and_resolved_topic() {
        let tool = MemoryForgetTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let request = tool
            .permission_request(&json!({"topic": "global:editor"}), Path::new("/irrelevant"))
            .unwrap();

        assert_eq!(request.action, ActionKind::PersistentMemory);
        assert_eq!(request.target, PermissionTarget::Other("global:editor".to_string()));
    }

    #[tokio::test]
    async fn execute_deletes_every_entry_and_reports_the_count() {
        let recall = Arc::new(aivyx_recall::InMemoryRecall::new());
        recall.put("global:notes", "one").await.unwrap();
        recall.put("global:notes", "two").await.unwrap();
        let tool = MemoryForgetTool::new(Arc::clone(&recall) as Arc<dyn Recall>);
        let dir = tempfile::tempdir().unwrap();

        let output = tool
            .execute(json!({"topic": "global:notes"}), &ctx(dir.path()))
            .await
            .unwrap();

        match output {
            ToolOutput::Ok(text) => assert!(text.contains("deleted 2 entries")),
            other => panic!("expected Ok, got {other:?}"),
        }
        assert_eq!(recall.get_recent("global:notes", 10).await.unwrap(), vec![]);
    }

    #[tokio::test]
    async fn execute_on_an_unwritten_topic_reports_zero_and_does_not_error() {
        let recall = Arc::new(aivyx_recall::InMemoryRecall::new());
        let tool = MemoryForgetTool::new(recall);
        let dir = tempfile::tempdir().unwrap();

        let output = tool
            .execute(json!({"topic": "global:never-written"}), &ctx(dir.path()))
            .await
            .unwrap();

        match output {
            ToolOutput::Ok(text) => assert!(text.contains("deleted 0 entries")),
            other => panic!("expected Ok, got {other:?}"),
        }
    }
}
```

`memory_forget` sorts alphabetically *before* `memory_read`/`memory_write` (both already present in `mod.rs` from Tasks 5-6). In `crates/aivyx-tools/src/tools/mod.rs`, insert `mod memory_forget;` between the `mcp_tool`/`memory_read` lines — i.e. move it ahead of the two already-added lines, not after them — and `pub use memory_forget::MemoryForgetTool;` at the matching position in the `pub use` block. The three lines should now read, in this exact order, in both blocks: `memory_forget`, `memory_read`, `memory_write`.

```rust
mod memory_forget;
```
```rust
pub use memory_forget::MemoryForgetTool;
```

Run: `cargo test -p aivyx-tools memory_forget -- --test-threads=1`
Expected: PASS (3 tests).

- [ ] **Step 2: Commit**

```bash
git add crates/aivyx-tools/src/tools/memory_forget.rs crates/aivyx-tools/src/tools/mod.rs
git commit -m "Add memory_forget tool

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 8: `aivyx-core::session::memory_dir_path()`

**Files:**
- Modify: `crates/aivyx-core/src/session.rs`

**Interfaces:**
- Produces: `pub fn memory_dir_path() -> Option<PathBuf>` — consumed by Task 9 (`agent_builder.rs`).

- [ ] **Step 1: Write the failing test**

Add to `crates/aivyx-core/src/session.rs`'s `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn memory_dir_path_is_a_memory_subdirectory_of_the_state_dir() {
        let path = memory_dir_path().expect("should resolve on any platform with a home dir");
        assert_eq!(path.file_name().unwrap(), "memory");
    }

    #[test]
    fn memory_dir_path_is_a_sibling_of_the_sessions_dir() {
        let memory = memory_dir_path().unwrap();
        let sessions = session_file_path(&std::env::current_dir().unwrap())
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert_eq!(memory.parent(), sessions.parent());
    }
```

Run: `cargo test -p aivyx-core memory_dir_path -- --test-threads=1`
Expected: FAIL to compile — `memory_dir_path` doesn't exist yet.

- [ ] **Step 2: Implement `memory_dir_path`**

Add to `crates/aivyx-core/src/session.rs`, immediately after `session_file_path`'s closing `}` (before the `fnv1a` helper):

```rust
/// Where cross-session memory (`memory_write`/`memory_read`/
/// `memory_forget`, via `aivyx-recall`'s `FileRecall`) is persisted: one
/// shared directory under the platform state dir, parallel to
/// `sessions/`. Unlike `session_file_path`, this directory isn't itself
/// project-keyed — `aivyx-tools`' topic-rewriting layer embeds a
/// project-scoping hash *inside* the topic string for `project:`-prefixed
/// topics instead (see `crates/aivyx-tools/src/memory_topic.rs`), so all
/// topics — global and per-project alike — share one directory of
/// per-topic files.
pub fn memory_dir_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "aivyx-coder")?;
    let state_dir = dirs
        .state_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dirs.data_local_dir().to_path_buf());
    Some(state_dir.join("memory"))
}
```

Run: `cargo test -p aivyx-core memory_dir_path -- --test-threads=1`
Expected: PASS (2 tests).

- [ ] **Step 3: Commit**

```bash
git add crates/aivyx-core/src/session.rs
git commit -m "Add session::memory_dir_path for aivyx-recall's FileRecall

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 9: Wire the three tools into `agent_builder.rs`

**Files:**
- Modify: `crates/aivyx/Cargo.toml`
- Modify: `crates/aivyx/src/agent_builder.rs`

**Interfaces:**
- Consumes: `aivyx_recall::{FileRecall, InMemoryRecall, Recall}` (Task 1/2), `session::memory_dir_path` (Task 8), `MemoryForgetTool`/`MemoryReadTool`/`MemoryWriteTool` (Tasks 5-7).

- [ ] **Step 1: Add the `aivyx-recall` dependency**

In `crates/aivyx/Cargo.toml`, add under `[dependencies]` (alphabetically before `aivyx-sandbox`):

```toml
aivyx-recall = { path = "../../../aivyx-recall" }
```

- [ ] **Step 2: Import the new tools**

In `crates/aivyx/src/agent_builder.rs`, the existing `use aivyx_tools::{...}` block already imports `McpToolAdapter, MoveFileTool` adjacently — insert the three new tool names alphabetically between them:

```rust
use aivyx_tools::{
    CommandSpec, DeleteFileTool, EditFileTool, FindReferencesTool, GetMcpPromptTool,
    GitBranchTool, GitCheckpointer, GitCommitTool, GitPrTool, GitPushTool, GitReadTool, GlobTool,
    GoToDefinitionTool, GrepTool, ListMcpPromptsTool, ListMcpResourcesTool, LspClient, McpClient,
    McpToolAdapter, MemoryForgetTool, MemoryReadTool, MemoryWriteTool, MoveFileTool, PatchFileTool,
    ReadFileTool, ReadMcpResourceTool, RememberPreferenceTool, ReplResizeTarget, ReplSendTool,
    ReplStartTool, ReplStopTool, RunCommandTool, RunShellTool, SetTasksTool, ToolExecutor,
    ToolRegistry, WebFetchTool, WebSearchTool, WriteFileTool, new_shared_repl_session,
};
```

- [ ] **Step 3: Construct the `Recall` and register the tools**

Insert immediately after the existing `registry.register(Arc::new(SetTasksTool::new(Arc::clone(&tasks))));` line:

```rust
    // Cross-session memory (memory_write/memory_read/memory_forget),
    // backed by aivyx-recall's FileRecall — one shared Arc<dyn Recall>
    // across all three tools. See docs/superpowers/specs/
    // 2026-08-09-aivyx-recall-design.md. Falls back to an in-memory-only
    // store (functional for this process, just not persisted) if the
    // state directory can't be resolved — matching session persistence's
    // own best-effort, never-block-startup posture immediately below.
    let recall: Arc<dyn aivyx_recall::Recall> = match session::memory_dir_path() {
        Some(dir) => Arc::new(aivyx_recall::FileRecall::new(dir)),
        None => {
            tracing::warn!(
                "no state directory available — cross-session memory will not persist across restarts"
            );
            Arc::new(aivyx_recall::InMemoryRecall::new())
        }
    };
    registry.register(Arc::new(MemoryReadTool::new(Arc::clone(&recall))));
    registry.register(Arc::new(MemoryWriteTool::new(Arc::clone(&recall))));
    registry.register(Arc::new(MemoryForgetTool::new(recall)));
```

- [ ] **Step 4: Build and run the existing `aivyx` test suite**

Run: `cargo build -p aivyx`
Expected: succeeds.

Run: `cargo test -p aivyx -- --test-threads=1`
Expected: PASS, no regressions (this task adds no new tests of its own — the tools' own behavior is already covered by Tasks 5-7's unit tests; this task is pure wiring, verified by the workspace compiling and existing `agent_builder`-adjacent tests, if any, still passing).

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx/Cargo.toml crates/aivyx/src/agent_builder.rs
git commit -m "Register memory_read/memory_write/memory_forget in agent_builder

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 10: Documentation — README + ROADMAP

**Files:**
- Modify: `README.md`
- Modify: `ROADMAP.md`

**Interfaces:** None — documentation only, no code interfaces.

- [ ] **Step 1: Add the three tools to README's tool table**

In `README.md`'s `## Tools` table (around line 804-829), add three rows after the `repl_stop` row (end of the table):

```markdown
| `memory_read` | recall entries saved under a topic (`global:`/`project:` scoped) | none (auto-allowed) |
| `memory_write` | persist a fact/note under a topic for future recall | prompt (then cacheable per topic) |
| `memory_forget` | delete every entry saved under a topic | prompt (then cacheable per topic) |
```

- [ ] **Step 2: Note the new tools in the autonomous-mode section**

Around README.md line 197-201 (the existing "Every MCP tool call and `remember_preference` are also unconditionally denied" sentence), extend it:

Find:
```
Every MCP tool call and `remember_preference` are also
unconditionally denied: there's no way to pre-approve an MCP tool the way
`allowed_commands` pre-approves a shell command, and `remember_preference`
edits a file whose effect isn't scoped to the current worktree the normal
Write/Delete boundary check bounds.
```

Replace with:
```
Every MCP tool call, `remember_preference`, and `memory_write`/
`memory_forget` are also unconditionally denied: there's no way to
pre-approve an MCP tool the way `allowed_commands` pre-approves a shell
command, and `remember_preference`/`memory_write`/`memory_forget` all
persist state whose effect isn't scoped to the current worktree the
normal Write/Delete boundary check bounds — unlike an ordinary in-worktree
edit, there's no checkpoint/rollback safety net to fall back on if an
unattended run gets it wrong.
```

- [ ] **Step 3: Add a "Cross-session memory" mention near "Learning over time"**

Around README.md line 327-331, after the existing `remember_preference` paragraph, add a new paragraph:

```markdown
**Cross-session memory**: beyond global preferences, the agent can save
smaller, incidental facts via `memory_write` — scoped to this project
(`project:`) or global (`global:`), recalled only when it explicitly
calls `memory_read` (never injected automatically). Unlike
`remember_preference`'s single always-active file, this is many small,
independently-forgettable notes — see `memory_forget`. Both writing and
forgetting go through the same review-then-cache flow as any other
mutating tool, and are unconditionally denied in autonomous mode for the
same reason `remember_preference` is (see above).
```

- [ ] **Step 4: Add a ROADMAP entry**

At the top of `ROADMAP.md`'s "## Current status" shipped list (immediately after the "**Slash command framework — shipped.**" paragraph, before "See `docs/HISTORY.md` for the full phase-by-phase narrative..."), add:

```markdown
**Cross-session memory (`aivyx-recall`) — shipped.** `memory_write`/
`memory_read`/`memory_forget` give the agent topic-scoped facts that
persist across sessions — global (`global:`) or project-scoped
(`project:`, keyed by the same cwd hash session persistence already
uses) — recalled only on an explicit `memory_read` call, never injected
ambiently. Backed by a new standalone crate/repo, `aivyx-recall`
(`Aivyx-Agent/aivyx-recall`), deliberately factored out so the sibling
Aivyx Personal Assistant can eventually adopt the same substrate instead
of reimplementing an equivalent one — see `docs/superpowers/specs/
2026-08-09-aivyx-recall-design.md`. A new `ActionKind::PersistentMemory`
backs the write/forget tools' gating: cacheable per exact topic in
interactive mode (unlike the existing, differently-shaped
`ActionKind::Memory` behind `remember_preference`), but unconditionally
denied under `--auto` for the same reason `remember_preference` already
is — both persist state outside the project working tree with no
checkpoint/rollback safety net.
```

- [ ] **Step 5: Update the "Last updated" date and commit**

Change `ROADMAP.md`'s `_Last updated: 2026-08-01_` line to the date this task is actually executed.

```bash
git add README.md ROADMAP.md
git commit -m "Document cross-session memory (aivyx-recall) in README and ROADMAP

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 11: Full verification sweep

**Files:** None — verification only.

- [ ] **Step 1: `aivyx-recall` — full test suite**

```bash
cd /home/julian/Projects/Rust/aivyx-recall
cargo test
cargo clippy --all-targets
```

Expected: all tests PASS, clippy clean (no warnings — this is a brand-new crate with no pre-existing warning baseline to compare against).

- [ ] **Step 2: `aivyx-coder` — full workspace test suite**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test --workspace -- --test-threads=1
```

Expected: PASS, no regressions anywhere in the workspace (473+ pre-existing tests, per `ROADMAP.md`, plus every test added in Tasks 1-9).

- [ ] **Step 3: `aivyx-coder` — clippy sweep**

```bash
cargo clippy --workspace --all-targets
```

Expected: no *new* warnings introduced by this chapter's changes (compare against a clean run on `main` before this branch if any pre-existing warnings are present, to isolate what's new).

- [ ] **Step 4: Manual smoke check of the open `checkpoint.rs` question from the spec**

The spec flagged an open question: does `GitCheckpointer::checkpoint` no-op harmlessly for `memory_write`, whose target is outside the project's git working tree? Confirm by reading `crates/aivyx-tools/src/checkpoint.rs`'s `checkpoint` method (already read during planning: it snapshots whatever tree is currently in `cwd`, deduplicating against `last_tree` — a `memory_write` call touches nothing inside `cwd`, so the tree is unchanged and the snapshot is skipped as a no-op). Write a short one-off manual note (not a new automated test — this is a property of `checkpoint.rs`'s existing dedup logic, already covered by that file's own tests) confirming this was checked, in the final PR/commit message for this task, e.g.:

> Confirmed against `checkpoint.rs`: `memory_write`'s checkpoint call is a cheap no-op (tree-oid dedup catches the unchanged `cwd` tree), not a new code path.

- [ ] **Step 5: Commit** (only if Steps 1-3 required any fixes; otherwise this task produces no diff and needs no commit)

```bash
git add -A
git commit -m "Fix workspace/clippy issues found during aivyx-recall verification sweep

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```
