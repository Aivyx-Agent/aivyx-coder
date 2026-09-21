# KV-Cache Multi-Process Slot Offset Design

## Context

`ROADMAP.md`'s "New backlog, from KV-cache persistence's (`aivyx-kvcache`)
task and final reviews" section lists a deliberately-deferred item:
`KvSlotPool`'s deterministic lowest-first checkout means two concurrent
`aivyx-coder` processes against one shared `llama-server` both check out
slot 0 first, pinning both sessions to the same physical slot and
mutually invalidating each other's cache. This spec fixes it, chosen as
this session's second small `ROADMAP.md` backlog item (queue item #4,
part 2), after the MCP cancellation/`cap_hit` pair.

This is a cache-*efficiency* problem, not a correctness one — nothing
crashes or misbehaves today; concurrent processes simply silently thrash
each other's KV-cache reuse instead of each getting a stable slot.

## Grounding

Read directly in the current codebase, not assumed:

- **`KvSlotPool` lives locally**, `crates/aivyx-llm/src/kv_slot_pool.rs` —
  not in the separate, pinned-git `aivyx-kvcache` repository (that crate
  is only the disk-persistence/HTTP-client layer,
  `LlamaServerSlotStore`). This fix is entirely within `aivyx-coder`,
  simpler in scope than the ROADMAP note's phrasing might suggest.
- **`KvSlotPool` is a pure, in-process `Mutex<HashSet<u32>>`** — zero
  cross-process awareness, by explicit design (its own doc comment: "No
  I/O, no knowledge of `aivyx-kvcache` at all"). `checkout()` is
  `(0..self.total_slots).find(|id| checked_out.insert(*id))` — always
  scans from 0, so a fresh process's first checkout is always slot 0.
  4 existing unit tests cover checkout/release/full-pool/no-op-release
  behavior, all order-sensitive (`checkout_returns_lowest_free_id_first`
  asserts `Some(0)` then `Some(1)` in that exact order) — any fix must
  keep these passing unmodified for the default (unlocked/single-process)
  case.
- **Constructed once per process**, `crates/aivyx/src/agent_builder.rs`
  (~line 890): `Arc::new(aivyx_llm::KvSlotPool::new(info.total_slots))`,
  where `info.total_slots` comes from a real `/props` probe against the
  configured `llama-server` — the actual physical slot count. This
  construction only happens when `settings.backend.kind ==
  BackendKind::LlamaServer` (excludes `LlamaServerBroker`, which has its
  own separate slot-admission mechanism via `aivyx-broker` — out of
  scope here, unaffected).
- **`settings.backend.resolved_kvcache_store_path()`** (`aivyx-config`)
  returns the on-disk KV-cache directory — defaults to a single, global
  `ProjectDirs`-derived path (`~/.local/share/aivyx-coder/kvcache`),
  **not parameterized by backend origin**. Two processes pointed at
  *different*, unrelated `llama-server` instances would still resolve to
  the same default store path unless the user configures
  `kvcache_store_path` differently per backend — meaning this path alone
  is not a safe scoping key for "processes that could actually collide on
  real physical slots." The actual `llama-server` origin (its base URL,
  already computed locally in `agent_builder.rs` as `origin`, passed to
  `LlamaServerSlotStore::open`) is the correct scoping key.
- **`fs2` (0.4.3) is already transitively present** in `Cargo.lock` (not
  yet a direct dependency of any workspace crate) — a stable,
  long-established cross-platform advisory-file-locking crate
  (`FileExt::try_lock_exclusive`/`unlock` on `std::fs::File`). Promoting
  it to a direct dependency avoids introducing an unfamiliar new crate
  into the dependency graph from scratch.
- **An existing precedent for stable, cross-version hashing**:
  `aivyx-core/src/session.rs` inlines its own FNV-1a hash specifically
  because `std::DefaultHasher` "explicitly does not guarantee" stability
  across program versions/builds. The same rationale (and the same
  inlined implementation) is the right tool for deriving a stable
  lock-directory name from a `llama-server` origin string.

## Decisions

**1. `KvSlotPool` stays pure — no file I/O added to it.** It gains one
new, optional construction parameter: an `offset: u32` (defaulting to `0`
via a plain `new(total_slots)` that's unchanged in behavior, plus a new
`with_offset(total_slots, offset)` constructor). `checkout()` becomes:

```rust
pub fn checkout(&self) -> Option<u32> {
    let mut checked_out = self.checked_out.lock().unwrap();
    (0..self.total_slots)
        .map(|i| (i + self.offset) % self.total_slots)
        .find(|id| checked_out.insert(*id))
}
```

Full slot coverage is preserved exactly (every slot is still tried
exactly once per full scan) — only the *starting point* changes. With
`offset = 0` this is byte-identical to the current implementation, so all
4 existing tests keep passing completely unmodified.

**2. A new, separate component, `SlotPoolLock`** (new file,
`crates/aivyx-llm/src/slot_pool_lock.rs`), owns the actual cross-process
coordination via real OS-level advisory file locks (`fs2`, promoted to a
direct `aivyx-llm` dependency). On construction, it tries
`try_lock_exclusive()` against files named `0.lock`, `1.lock`, ... up to
`total_slots - 1` inside a caller-supplied lock directory, in order,
taking the first one that succeeds as its claimed offset. The lock is
held via the open `File` handle for the struct's lifetime and released
automatically on `Drop` — **and automatically on process crash too**,
since `flock`-style advisory locks are kernel-held, not something
requiring explicit cleanup or a stale-PID-file staleness check. If every
index is already locked (i.e. real concurrent-process count has reached
or exceeded `total_slots`), falls back to offset `0` — no worse than
today's behavior, and additional coordination genuinely can't help once
the pool is already fully oversubscribed.

**3. The lock directory is scoped by the `llama-server` origin, not just
the generic store path**: `<resolved_kvcache_store_path>/locks/<fnv1a-hex-of-origin>/`,
reusing `session.rs`'s exact inlined FNV-1a implementation (moved to a
small shared location both files can use, or duplicated with a comment
cross-referencing the original — implementation detail for the plan to
decide) for the same cross-version-stability rationale. Processes
pointed at different `llama-server` origins get independent lock
directories and never contend with each other; processes sharing the
same origin (the actual collision scenario the ROADMAP note describes)
correctly coordinate.

**4. Wiring in `agent_builder.rs`**: immediately before the existing
`KvSlotPool::new(info.total_slots)` call, construct a `SlotPoolLock`
(directory derived per Decision 3, `total_slots` from the same
`info.total_slots` already in scope) and pass its offset into
`KvSlotPool::with_offset(info.total_slots, lock.offset())` instead. The
`SlotPoolLock` itself must be kept alive for the process's lifetime
(stored alongside the other `kv_cache_handles` tuple fields, or leaked
intentionally with a comment — implementation detail for the plan), since
dropping it early would release the claim while the process is still
running.

## What this spec does not decide

- Any change to `aivyx-broker`'s own, separate slot-admission mechanism
  (`BackendKind::LlamaServerBroker`) — unaffected, out of scope.
- Any change to `LlamaServerSlotStore` or the on-disk cache-entry format
  itself (`aivyx-kvcache`, the separate pinned repo) — this fix is
  entirely about in-memory slot *selection order*, not persistence.
- Cleanup of stale lock-directory files themselves (the empty `N.lock`
  files persist on disk indefinitely, harmlessly, after a process exits —
  only the OS-level lock they hold is released) — not a real problem
  worth solving (a handful of empty files per distinct origin ever
  contacted), and pruning them safely while other processes might still
  reference the same directory adds real complexity for no functional
  benefit.
- Any behavior change when only one `aivyx-coder` process is running
  against a given `llama-server` — the fix is inert in that case (offset
  always resolves to `0`, since index `0` is always available to claim
  first).
