# KV-Cache Multi-Process Slot Offset Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two concurrent `aivyx-coder` processes sharing one `llama-server` no longer both deterministically claim physical KV-cache slot 0 first — each process claims a distinct starting offset via real cross-process file locking, so they land on different slots instead of mutually invalidating each other's cache.

**Architecture:** `KvSlotPool` (`aivyx-llm`) stays pure (no I/O) and gains an `offset` its `checkout()` scans from instead of always starting at 0. A new, separate component, `SlotPoolLock` (`aivyx-llm`), does the actual cross-process coordination via real OS-level advisory file locks (`fs2`), scoped per-`llama-server`-origin. `agent_builder.rs` wires the two together at the one place `KvSlotPool` is already constructed.

**Tech Stack:** Rust, `fs2` (already transitively present, promoted to a direct `aivyx-llm` dependency), `std::fs` advisory file locking.

## Global Constraints

- `KvSlotPool` itself gets zero file I/O added — the pure/impure separation its own existing doc comment already states is preserved.
- With `offset = 0` (or via the unchanged `KvSlotPool::new`), `checkout()`'s behavior must be byte-identical to today — all 4 existing `kv_slot_pool.rs` tests must pass completely unmodified.
- `aivyx-broker`'s own separate slot-admission path (`BackendKind::LlamaServerBroker`) is untouched.
- No change to `aivyx-kvcache` (the separate pinned repo) or to `LlamaServerSlotStore`.
- File-scoped `rustfmt --edition 2024 --check <path>` / `rustfmt --edition 2024 <path>` only — **never** a package-scoped `cargo fmt -p <crate>` command with no file argument, per this project's own repeated, documented incident history.

---

### Task 1: `KvSlotPool` gains an offset

**Files:**
- Modify: `crates/aivyx-llm/src/kv_slot_pool.rs`

**Interfaces:**
- Produces: `KvSlotPool::with_offset(total_slots: u32, offset: u32) -> Self` — consumed by Task 2's wiring in `agent_builder.rs`. `KvSlotPool::new(total_slots: u32) -> Self` keeps its exact existing signature and behavior (implemented in terms of `with_offset(total_slots, 0)`).

- [ ] **Step 1: Add the `offset` field and `with_offset` constructor, update `checkout`**

In `crates/aivyx-llm/src/kv_slot_pool.rs`, find this exact block:

```rust
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
        (0..self.total_slots).find(|id| checked_out.insert(*id))
    }
```

Replace it with:

```rust
pub struct KvSlotPool {
    total_slots: u32,
    offset: u32,
    checked_out: Mutex<HashSet<u32>>,
}

impl KvSlotPool {
    /// Equivalent to `with_offset(total_slots, 0)` — the pool's original,
    /// single-process behavior (always scans starting from slot 0).
    pub fn new(total_slots: u32) -> Self {
        Self::with_offset(total_slots, 0)
    }

    /// Like `new`, but `checkout()` scans starting from `offset` instead
    /// of always from slot 0 — lets a caller (see `SlotPoolLock`) give
    /// concurrent processes sharing one physical `llama-server` distinct
    /// starting points, so they land on different slots instead of all
    /// racing for slot 0 first. Full slot coverage is unaffected: every
    /// slot is still tried exactly once per full scan, only the order
    /// changes. `offset` wraps via `%`, so any `u32` value (including one
    /// `>= total_slots`) is safe to pass.
    pub fn with_offset(total_slots: u32, offset: u32) -> Self {
        Self {
            total_slots,
            offset: if total_slots == 0 { 0 } else { offset % total_slots },
            checked_out: Mutex::new(HashSet::new()),
        }
    }

    /// Returns the lowest-numbered free slot id starting from `offset`
    /// (wrapping), or `None` if every slot is already checked out.
    /// Deterministic ordering makes pool behavior predictable in tests;
    /// no particular ordering is required for correctness beyond full
    /// coverage.
    pub fn checkout(&self) -> Option<u32> {
        let mut checked_out = self.checked_out.lock().unwrap();
        (0..self.total_slots)
            .map(|i| (i + self.offset) % self.total_slots)
            .find(|id| checked_out.insert(*id))
    }
```

- [ ] **Step 2: Add tests for offset behavior**

In `crates/aivyx-llm/src/kv_slot_pool.rs`'s test module, immediately after the existing `releasing_a_never_checked_out_id_is_a_silent_no_op` test's closing `}`, add:

```rust
    #[test]
    fn with_offset_starts_checkout_from_the_given_slot() {
        let pool = KvSlotPool::with_offset(4, 2);
        assert_eq!(pool.checkout(), Some(2));
        assert_eq!(pool.checkout(), Some(3));
        assert_eq!(pool.checkout(), Some(0), "must wrap around past the end");
        assert_eq!(pool.checkout(), Some(1));
        assert_eq!(pool.checkout(), None, "still rejects a 5th concurrent checkout");
    }

    #[test]
    fn with_offset_zero_matches_new_exactly() {
        let via_new = KvSlotPool::new(4);
        let via_offset = KvSlotPool::with_offset(4, 0);
        assert_eq!(via_new.checkout(), Some(0));
        assert_eq!(via_offset.checkout(), Some(0));
        assert_eq!(via_new.checkout(), Some(1));
        assert_eq!(via_offset.checkout(), Some(1));
    }

    #[test]
    fn with_offset_wraps_an_offset_greater_than_total_slots() {
        // offset 6 against 4 total slots must behave identically to
        // offset 2 (6 % 4 == 2), not panic or index out of range.
        let pool = KvSlotPool::with_offset(4, 6);
        assert_eq!(pool.checkout(), Some(2));
    }
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p aivyx-llm kv_slot_pool -- --nocapture`
Expected: all tests pass — the 4 pre-existing ones (assertions and behavior completely unchanged) plus the 3 new offset tests.

- [ ] **Step 4: Format and commit**

```bash
rustfmt --edition 2024 crates/aivyx-llm/src/kv_slot_pool.rs
git add crates/aivyx-llm/src/kv_slot_pool.rs
git commit -m "feat: KvSlotPool supports a starting checkout offset"
```

---

### Task 2: `SlotPoolLock` + wiring in `agent_builder.rs`

**Files:**
- Create: `crates/aivyx-llm/src/slot_pool_lock.rs`
- Modify: `crates/aivyx-llm/src/lib.rs` (export the new module)
- Modify: `crates/aivyx-llm/Cargo.toml` (add `fs2` as a direct dependency)
- Modify: `crates/aivyx/src/agent_builder.rs` (wire `SlotPoolLock` into the existing `KvSlotPool` construction)

**Interfaces:**
- Consumes: `KvSlotPool::with_offset` (Task 1).
- Produces: `SlotPoolLock::acquire(lock_dir: &Path, slot_count: u32) -> io::Result<SlotPoolLock>`, `SlotPoolLock::offset(&self) -> u32` — consumed only by `agent_builder.rs`'s wiring step in this same task.

- [ ] **Step 1: Add `fs2` as a direct dependency**

In `crates/aivyx-llm/Cargo.toml`, find this exact line:

```toml
eventsource-stream = "0.2.3"
```

Replace it with:

```toml
eventsource-stream = "0.2.3"
fs2 = "0.4.3"
```

(Matches the version already present transitively in `Cargo.lock` — confirm with `cargo tree -p fs2` after this step that only one `fs2` version resolves, not two.)

- [ ] **Step 2: Write `SlotPoolLock`**

Create `crates/aivyx-llm/src/slot_pool_lock.rs`:

```rust
//! Cross-process coordination for `KvSlotPool`'s starting offset (see that
//! module's own doc comment for why this lives in a separate file --
//! `KvSlotPool` itself stays pure, no I/O). Uses real OS-level advisory
//! file locks (`fs2`), so a claimed offset is automatically released on
//! process exit *or crash* -- no stale-lock cleanup needed, unlike a
//! PID-file-based scheme.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

use fs2::FileExt;

/// Holds an exclusive OS-level lock on one `<offset>.lock` file inside a
/// lock directory, for as long as this value is alive -- dropping it
/// releases the lock (`std::fs::File`'s own `Drop` closes the descriptor,
/// which releases an `flock`-style advisory lock automatically). Callers
/// that need the claim to last the whole process should `Box::leak` the
/// returned value rather than let it drop early -- see `agent_builder.rs`.
pub struct SlotPoolLock {
    _file: File,
    offset: u32,
}

impl SlotPoolLock {
    /// Tries `0.lock`, `1.lock`, ... up to `slot_count - 1` inside
    /// `lock_dir` (created if it doesn't exist), taking the first index
    /// this process can exclusively lock. Returns an error if every index
    /// is already claimed by another process, or if the lock directory
    /// can't be created/a lock file can't be opened -- callers should
    /// treat any error here as "fall back to offset 0," matching today's
    /// un-coordinated behavior, since this is a cache-efficiency
    /// optimization, not something worth failing agent startup over.
    pub fn acquire(lock_dir: &Path, slot_count: u32) -> io::Result<Self> {
        fs::create_dir_all(lock_dir)?;
        for offset in 0..slot_count.max(1) {
            let path = lock_dir.join(format!("{offset}.lock"));
            let file = OpenOptions::new().create(true).write(true).open(&path)?;
            if file.try_lock_exclusive().is_ok() {
                return Ok(Self { _file: file, offset });
            }
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "every slot-pool lock index is already claimed by another process",
        ))
    }

    pub fn offset(&self) -> u32 {
        self.offset
    }
}

/// FNV-1a, inlined because the lock-directory name must be stable across
/// program versions -- `std`'s `DefaultHasher` explicitly does not
/// guarantee that. Duplicated (not shared) from `aivyx-core::session`'s
/// identical private function, since the two live in different crates and
/// this one value doesn't warrant a new shared-utility crate.
pub fn fnv1a(bytes: &[u8]) -> u64 {
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

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aivyx-slot-pool-lock-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir
    }

    #[test]
    fn first_claim_gets_offset_zero() {
        let dir = unique_temp_dir("first");
        let lock = SlotPoolLock::acquire(&dir, 4).expect("first claim must succeed");
        assert_eq!(lock.offset(), 0);
    }

    #[test]
    fn a_second_concurrent_claim_gets_a_different_offset() {
        // `flock`-style advisory locks are held per OPEN FILE DESCRIPTION,
        // not per-process -- two separate `File::open`/lock calls, even
        // from the same test process, genuinely contend with each other.
        // This is documented `flock()` behavior on Linux/macOS (this
        // project's real supported platforms); if this test fails, that
        // assumption -- not the test itself -- is the first thing to
        // re-check.
        let dir = unique_temp_dir("second");
        let first = SlotPoolLock::acquire(&dir, 4).expect("first claim must succeed");
        let second = SlotPoolLock::acquire(&dir, 4).expect("second claim must succeed");
        assert_eq!(first.offset(), 0);
        assert_eq!(second.offset(), 1);
    }

    #[test]
    fn releasing_a_claim_frees_its_offset_for_reuse() {
        let dir = unique_temp_dir("release");
        let first = SlotPoolLock::acquire(&dir, 2).expect("first claim must succeed");
        assert_eq!(first.offset(), 0);
        drop(first);
        let second = SlotPoolLock::acquire(&dir, 2).expect("second claim must succeed");
        assert_eq!(second.offset(), 0, "dropping the first lock must release offset 0");
    }

    #[test]
    fn errors_once_every_index_is_already_claimed() {
        let dir = unique_temp_dir("exhausted");
        let _first = SlotPoolLock::acquire(&dir, 1).expect("first claim must succeed");
        let second = SlotPoolLock::acquire(&dir, 1);
        assert!(
            second.is_err(),
            "with slot_count 1, a second concurrent claim must fail, not silently reuse offset 0"
        );
    }

    #[test]
    fn fnv1a_is_deterministic_for_the_same_input() {
        assert_eq!(fnv1a(b"http://localhost:8080"), fnv1a(b"http://localhost:8080"));
        assert_ne!(fnv1a(b"http://localhost:8080"), fnv1a(b"http://localhost:8081"));
    }
}
```

- [ ] **Step 3: Export the new module**

In `crates/aivyx-llm/src/lib.rs`, find this exact line:

```rust
mod kv_slot_pool;
```

Replace it with:

```rust
mod kv_slot_pool;
mod slot_pool_lock;
```

Then find this exact line:

```rust
pub use kv_slot_pool::KvSlotPool;
```

Replace it with:

```rust
pub use kv_slot_pool::KvSlotPool;
pub use slot_pool_lock::{SlotPoolLock, fnv1a};
```

(`kv_slot_pool.rs` uses a private `mod` plus individual `pub use` re-exports, not `pub mod` — mirror that exact pattern for the new module, so both `aivyx_llm::SlotPoolLock` and `aivyx_llm::fnv1a` are reachable directly, not via `aivyx_llm::slot_pool_lock::...`.)

- [ ] **Step 4: Verify `aivyx-llm` compiles and its new tests pass**

Run: `cargo test -p aivyx-llm slot_pool_lock -- --nocapture`
Expected: all 5 new tests pass. If `a_second_concurrent_claim_gets_a_different_offset` fails, do not weaken the assertion — this would mean `fs2`'s lock semantics on this platform differ from the documented per-open-file-description behavior assumed here, which is a real finding to report (BLOCKED), not something to work around by changing the test.

- [ ] **Step 5: Wire `SlotPoolLock` into `agent_builder.rs`**

In `crates/aivyx/src/agent_builder.rs`, find this exact block:

```rust
                                // Single source of truth for the effective path
                                // (configured override, or the historical
                                // ProjectDirs-derived default) — see
                                // BackendSettings::resolved_kvcache_store_path in
                                // aivyx-config, also reused by Settings::effective_deny_paths
                                // below so the two can never silently diverge.
                                let store_path = settings.backend.resolved_kvcache_store_path();
                                match aivyx_kvcache::LlamaServerSlotStore::open(
                                    &store_path,
                                    origin, // NOT settings.backend.base_url -- /slots is a native
                                    // llama-server endpoint at the origin, not under /v1
                                    // (confirmed live: a /v1-prefixed base_url 404s on
                                    // /v1/slots/{id}?action=save, since the real path is
                                    // just /slots/{id}?action=save)
                                    settings.backend.kvcache_max_bytes,
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
```

Replace it with:

```rust
                                // Single source of truth for the effective path
                                // (configured override, or the historical
                                // ProjectDirs-derived default) — see
                                // BackendSettings::resolved_kvcache_store_path in
                                // aivyx-config, also reused by Settings::effective_deny_paths
                                // below so the two can never silently diverge.
                                let store_path = settings.backend.resolved_kvcache_store_path();
                                match aivyx_kvcache::LlamaServerSlotStore::open(
                                    &store_path,
                                    origin, // NOT settings.backend.base_url -- /slots is a native
                                    // llama-server endpoint at the origin, not under /v1
                                    // (confirmed live: a /v1-prefixed base_url 404s on
                                    // /v1/slots/{id}?action=save, since the real path is
                                    // just /slots/{id}?action=save)
                                    settings.backend.kvcache_max_bytes,
                                ) {
                                    Ok(store) => {
                                        // Scoped by origin (not just store_path, which
                                        // defaults to one global directory regardless of
                                        // which llama-server is configured) -- two
                                        // processes pointed at different, unrelated
                                        // llama-server instances must not contend with
                                        // each other for an offset neither can use to
                                        // help the other.
                                        let lock_dir = store_path
                                            .join("locks")
                                            .join(format!("{:016x}", aivyx_llm::fnv1a(origin.as_bytes())));
                                        let offset = match aivyx_llm::SlotPoolLock::acquire(
                                            &lock_dir,
                                            info.total_slots,
                                        ) {
                                            Ok(lock) => {
                                                let offset = lock.offset();
                                                // Held for the process's lifetime --
                                                // dropping it early would release the
                                                // claim while this process is still
                                                // running. See SlotPoolLock's own doc
                                                // comment.
                                                Box::leak(Box::new(lock));
                                                offset
                                            }
                                            Err(err) => {
                                                tracing::warn!(error = %err, "kvcache: failed to acquire a slot-pool lock; using offset 0");
                                                0
                                            }
                                        };
                                        Some((
                                            Arc::new(aivyx_llm::KvSlotPool::with_offset(
                                                info.total_slots,
                                                offset,
                                            )),
                                            Arc::new(store),
                                            info.build_info,
                                        ))
                                    }
                                    Err(err) => {
                                        tracing::warn!(error = %err, "kvcache: failed to open store; disabled for this run");
                                        None
                                    }
                                }
```

- [ ] **Step 6: Verify `aivyx` compiles**

Run: `cargo check -p aivyx`
Expected: compiles cleanly.

- [ ] **Step 7: Format the exact files touched, and build/test/lint the full workspace**

Run:
```bash
rustfmt --edition 2024 crates/aivyx-llm/src/lib.rs
rustfmt --edition 2024 crates/aivyx-llm/Cargo.toml
rustfmt --edition 2024 crates/aivyx/src/agent_builder.rs
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```
(`rustfmt` on a `.toml` file is a no-op/harmless if attempted — skip it if `rustfmt` rejects non-Rust input, and instead just visually confirm the one added line in Step 1 matches the surrounding file's existing formatting style.)
Expected: `cargo build`/`cargo test` succeed with zero failures; `cargo clippy` reports zero warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx-llm/src/slot_pool_lock.rs crates/aivyx-llm/src/lib.rs crates/aivyx-llm/Cargo.toml crates/aivyx/src/agent_builder.rs
git commit -m "feat: coordinate KV-cache slot offsets across concurrent aivyx-coder processes"
```

---

## Self-Review Notes

**Spec coverage:** Decision 1 (`KvSlotPool` stays pure, gains `offset`/`with_offset`, full coverage preserved) → Task 1. Decision 2 (`SlotPoolLock`, real `fs2`-based advisory locks, auto-release on drop/crash, fallback to offset 0) → Task 2 Steps 1-2. Decision 3 (lock directory scoped by a stable FNV-1a hash of origin, mirroring `session.rs`'s rationale) → Task 2 Step 2's `fnv1a` function and Step 5's `lock_dir` construction. Decision 4 (wiring in `agent_builder.rs`, `SlotPoolLock` kept alive via intentional leak) → Task 2 Step 5. "What this spec does not decide" items are all genuinely untouched: no `aivyx-broker` change, no `aivyx-kvcache`/`LlamaServerSlotStore` change, no lock-file cleanup mechanism, single-process behavior unchanged (offset always resolves to 0 when only one process is running, confirmed by Task 2's own `first_claim_gets_offset_zero` test).

**Global Constraints deviation:** none — `KvSlotPool` gets no I/O added (all file-locking lives in the new, separate `SlotPoolLock`), `offset = 0`/`KvSlotPool::new` behavior is proven byte-identical by Task 1's own tests, `aivyx-broker`/`aivyx-kvcache` are untouched, only file-scoped `rustfmt` is used.

**Placeholder scan:** no TBD/TODO; every step shows complete, real code (full struct/impl bodies, full test code, the exact wiring diff); Task 2 Step 3 is deliberately investigative ("check the exact existing pattern first") rather than a placeholder — it names precisely what to look for and why, not a vague "figure it out."

**Type/interface consistency check:** `KvSlotPool::with_offset(total_slots: u32, offset: u32)` (Task 1) is called with exactly that signature at Task 2 Step 5's `Arc::new(aivyx_llm::KvSlotPool::with_offset(info.total_slots, offset))`. `SlotPoolLock::acquire(lock_dir: &Path, slot_count: u32) -> io::Result<Self>` and `.offset() -> u32` (Task 2 Step 2) are consumed with matching types at Task 2 Step 5. `fnv1a(bytes: &[u8]) -> u64` (Task 2 Step 2) is called with `origin.as_bytes()` (Task 2 Step 5, where `origin: &str` is already in scope from the surrounding existing code) — types match.
