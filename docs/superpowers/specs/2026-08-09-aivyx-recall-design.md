# `aivyx-recall` — Cross-Session Memory Substrate — Design

**Status:** Approved by user 2026-08-09.

## Context

This chapter opened from an ecosystem-level question — the user asked
whether "RAG" and "Context-Memory" would be worth building as standalone,
shared features across the Aivyx Ecosystem (`aivyx`, the personal
assistant, and `aivyx-coder`, this project). Investigating both terms
against the actual state of both repos reshaped the question:

- **RAG for code** was already evaluated and explicitly rejected in this
  project. `docs/HISTORY.md` cites evidence (["grep beat
  embeddings"](https://jxnl.co/writing/2025/09/11/why-grep-beat-embeddings-in-our-swe-bench-agent-lessons-from-augment/))
  for why embeddings/vector search on code lose to exact search + the
  tree-sitter repo map already shipped. Nothing here revisits that
  decision.
- **Cross-session memory** (facts/preferences/procedures persisting
  across sessions) already exists in `aivyx` — `crates/aivyx-memory`, a
  mature ~8,300-line crate: topic-scoped entries, BM25 lexical search, a
  hand-rolled ANN vector index, redb-backed AEAD-encrypted persistence,
  wired through capability scopes and HMAC audit as an explicit tool call
  ("memory is a tool, not an ambient system" is a stated design contract
  in that crate's own module docs). `aivyx-coder` has no equivalent —
  session persistence here is conversation replay only, no learned
  cross-session state.

This chapter builds cross-session memory for `aivyx-coder`, backed by a
new standalone crate (`aivyx-recall`) so the substrate is genuinely
shared rather than duplicated — but does **not** attempt to migrate
`aivyx`'s existing, mature memory system onto it today. That's real
refactor risk against production code this repo's own test suite can't
verify; it's recorded as deferred scope (see "Deferred" below) for a
future session rooted in the `aivyx` repo itself.

`aivyx-memory`'s own module docs describe the `Memory` trait and its
`InMemoryMemory` fake as already "substrate-agnostic" — i.e. the
generic part this chapter extracts was already architecturally
separated from the redb/encryption/capability-scope layers wrapped
around it, which is why extracting a clean substrate crate is tractable
even though the whole crate is not something to migrate lightly.

## Decisions

### The crate: `aivyx-recall`

New repo, `Aivyx-Agent/aivyx-recall` (local-first for now — created and
committed like any other repo here, pushed to GitHub whenever that's
convenient, same as this project's own early history). Named distinctly
from `aivyx`'s internal `aivyx-memory` crate to avoid confusion between
"the shared substrate" and "the personal assistant's own memory system."

```rust
#[async_trait]
pub trait Recall: Send + Sync {
    /// Store one entry under `topic`. Returns the assigned `seq`.
    /// Fails fast on an empty topic.
    async fn put(&self, topic: &str, body: &str) -> Result<u64, RecallError>;

    /// Up to `limit` entries for `topic`, newest first (seq descending).
    /// An unwritten topic returns an empty Vec, not an error. Fails on
    /// empty topic or limit == 0.
    async fn get_recent(&self, topic: &str, limit: usize) -> Result<Vec<RecallEntry>, RecallError>;

    /// Delete every entry under `topic`. Returns the count deleted (0 is
    /// a valid no-op, not an error). Fails fast on empty topic.
    async fn forget(&self, topic: &str) -> Result<usize, RecallError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallEntry {
    pub topic: String,
    pub body: String,
    pub seq: u64,             // monotonic per topic, substrate-assigned
    pub created_at_secs: u64, // wall clock, substrate-assigned
}

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
```

This is deliberately the same three-method, topic+seq contract
`aivyx-memory`'s `Memory` trait already proved out in production, minus
everything that's `aivyx`-specific accretion rather than generic
substrate: no embeddings/ANN, no LRU-by-last-read eviction, no
session-namespace wildcard prefix-walks, no reserved internal-topic
constants, no single-entry delete-by-seq. Any of those can be added
later — by either consumer, or by promoting one into the shared crate if
it proves generically useful — without breaking this contract (new
fields land `#[serde(default)]`, matching `aivyx-memory`'s own precedent
for `last_read_at_secs`).

**No dependency on `aivyx-storage`/`aivyx-crypto`/`aivyx-capability`.**
This is the whole point of extracting a standalone crate: `aivyx-coder`
can depend on it directly with zero security-model baggage, and `aivyx`
can later wrap it with its own encryption/capability-scope/audit layer
(a `RedbRecall: Recall` impl) without this crate ever needing to know
those concepts exist.

### Implementations shipped in the crate

- **`InMemoryRecall`** — deterministic in-process fake, no persistence.
  For unit tests in this crate and both consumers.
- **`FileRecall`** — the default persistent backend. One JSON file per
  topic, under a caller-supplied directory. Filename = sanitized-topic
  prefix + FNV-1a hash of the full topic string — the exact scheme
  `crates/aivyx-core/src/session.rs`'s `fnv1a`/`session_file_path`
  already uses for session keys (inlined FNV-1a, no new dependency,
  chosen there specifically because `std`'s `DefaultHasher` doesn't
  guarantee cross-version stability). Written via `std::fs::write` then
  `chmod 0600` on Unix, matching `session.rs::save`'s exact pattern
  (direct write, not atomic-via-tempfile-rename — accepted as
  best-effort there, and the same tradeoff applies here: a torn write
  from a mid-write crash is rare and the failure mode is "lose that
  topic's file," not corruption spreading elsewhere). A single
  in-process `tokio::sync::Mutex` serializes all read-modify-write
  operations across every topic — simple and correct at personal-memory
  scale; no per-topic lock bookkeeping needed.

### `aivyx-coder` integration

Three new tools in `aivyx-tools/src/tools/`, each a thin wrapper over an
`Arc<dyn Recall>` added to `ToolContext` (same pattern as every other
shared collaborator — `Arc<dyn LlmProvider>`, `Arc<dyn Storage>`, etc.):

| Tool | Args | `ActionKind` | Gate behavior |
|---|---|---|---|
| `memory_write` | `topic`, `body` | `PersistentMemory` (new — see addendum) | Confirm on first use per distinct topic; Always-Allow caches on the **topic string** — same exact-target-caching principle `write_file`/`git_commit` already use, so approving `project:flaky-tests` never blesses `project:other-topic`. Unconditionally denied under `--auto`. |
| `memory_read` | `topic`, `limit` | `Read` | Auto-allow, like `read_file` — recall is never a mutation. Unaffected by `--auto`. |
| `memory_forget` | `topic` | `PersistentMemory` (new — see addendum) | Confirm, same caching as `memory_write`. Unconditionally denied under `--auto`. |

**Why `memory_write` isn't `Internal` (auto-allow) like `set_tasks`:**
`set_tasks` is ephemeral, current-session-only bookkeeping — it's
honest to auto-allow because nothing persists past the conversation.
Memory is the opposite: content the model chooses to write persists
across *every future session* and (per the Surfacing decision below) is
only ever re-read via an explicit `memory_read` call the model itself
issues — but that still means a manipulated model could plant content
today that a differently-manipulated (or the same) model reads back and
acts on next week, with the write itself never having been visible to
the user if it were auto-allowed. Confirm-once-per-topic, Always-Allow
cacheable, balances that real replay risk against not nagging on every
note once a topic is trusted.

**Topic namespacing — global vs. project-scoped, one substrate:** the
model must prefix every topic it writes or reads with `global:` or
`project:` (tool descriptions teach this; a bare topic returns a clear
validation error steering the model to pick one). The tool layer
transparently rewrites `project:<rest>` to an internal storage key
`project:<cwd-hash>:<rest>` before it ever reaches `Recall`, reusing the
exact same canonicalized-cwd FNV-1a hash `session_file_path` already
computes for session keys — so the same nominal topic in two different
project directories never collides, and `Recall` itself stays completely
scope-oblivious (it only ever sees an opaque topic string), matching
`aivyx-memory`'s own validated "session-oblivious substrate, scope lives
one layer up" design.

**Surfacing model — on-demand only, no ambient injection.** The model
sees remembered facts only when it explicitly calls `memory_read`, never
via automatic splicing into the system prompt. This matches
`aivyx-memory`'s own deliberate contract ("memory is a tool, not an
ambient system") and keeps every recall visible and auditable in the
transcript, rather than reopening the untagged-content-reenters-context
risk this project's README already flags as a known limitation for
file/command output.

**Storage location:** `~/.local/state/aivyx-coder/memory/`, parallel to
the existing `sessions/` directory (same `directories::ProjectDirs`
state dir).

### Addendum — relationship to `remember_preference`, and autonomous mode (`--auto`)

Found while writing the implementation plan, not during the original
brainstorm: `aivyx-coder` already has a cross-session, global memory
mechanism — `remember_preference`
(`crates/aivyx-tools/src/tools/remember_preference.rs`,
`docs/superpowers/specs/2026-07-21-agent-learned-preferences-design.md`),
which lets the model propose a full rewrite of
`~/.config/aivyx-coder/AGENTS.md`, a file already ambiently injected
into every turn's system prompt across every project. This doesn't
duplicate what this chapter builds: `remember_preference` is *ambient*
(always injected, no recall call needed) and *whole-document* (propose
one complete replacement, reviewed as one diff), aimed at "how you like
me to work" instructions meant to be permanently active. `memory_write`/
`memory_read`/`memory_forget` are *on-demand* (nothing enters context
until the model calls `memory_read`) and *topic-scoped* (one small fact
per entry, no full-document rewrite), aimed at incidental facts that
don't need to be always-on. Both this chapter's `global:` and `project:`
namespaces are therefore intentional, not overlapping with
`remember_preference`'s existing global file.

`remember_preference` also introduced `ActionKind::Memory`
(`crates/aivyx-sandbox/src/lib.rs`) — relevant precedent, but **not**
directly reusable here. It's shaped narrowly for `remember_preference`'s
specific danger: its `PermissionTarget::Other` description is a fixed
constant string regardless of the content actually being proposed, so
`ConfirmationGate::check` treats it as *never* Always-Allow-cached (a
cached approval would silently bless every future, unreviewed rewrite)
and *unconditionally denied* under `--auto` (the autonomous-mode branch
checks `action == ActionKind::Memory` directly).

This surfaced a real gap the original design missed: `--auto` was never
addressed for `memory_write`/`memory_forget`. Both persist content
outside the project working tree, with no git checkpoint/rollback
safety net — the same property that motivated `ActionKind::Memory`'s
unconditional `--auto` denial. Confirmed by reading
`ConfirmationGate::check`'s autonomous-mode branch: today, a `Write`/
`Delete` action on a `PermissionTarget::Other` target falls through to
silent auto-allow unless its `ActionKind` is one of the two kinds
special-cased for unconditional denial (`McpTool`, `Memory`) — so
`memory_write`/`memory_forget` as originally specified (plain `Write`/
`Delete`) would have silently auto-allowed, unattended, under `--auto`.

**Resolution:** a new `ActionKind::PersistentMemory`, used by both
`memory_write` and `memory_forget` (replacing the plain `Write`/
`Delete` kinds in the table above). It gets the same unconditional
`--auto` denial as `McpTool`/`Memory` (add it alongside them in
`ConfirmationGate::check`'s autonomous-mode branch), but — unlike
`Memory` — is *not* added to the `never_cached` check, since this
chapter's targets (topic strings) genuinely vary per call and per-topic
Always-Allow caching is safe and intended, unlike `remember_preference`'s
fixed-string case. `memory_read` is unaffected (stays `Read`, always
auto-allowed, `--auto` included).

**Default posture:** on by default, no new `[memory]` config toggle.
Consistent with how every other tool in this project ships — the
permission gate (confirm + Always-Allow-per-topic) is the actual safety
mechanism, not a config flag a user has to discover. This is a
deliberate departure from the `[editor_context]`/`[editor_approval]`
precedent, which gated features that are silently inert without an
external process — memory has no such inertness condition, so a config
flag would just be a second on/off switch duplicating what the gate
already provides.

### Open question for the implementation plan

How `memory_write`'s `mutates_outside_session() == true` interacts with
`ToolExecutor::dispatch`'s existing git-checkpoint step (any tool with
`mutates_outside_session() == true` is checkpointed via
`refs/aivyx/checkpoints/<ts>` before execution). Memory storage lives
outside the project's git working tree entirely — under
`~/.local/state/aivyx-coder/memory/`, not the project — for the same
reason session files live outside the project (file contents/command
output must never end up committed to the project's own history). A git
snapshot of the *project* repo doesn't obviously apply to a mutation
that touches no file inside it. `mutates_outside_session()` should still
return `true` so Plan mode correctly hides `memory_write` (plan mode is
read-only investigation; permanently remembering a new fact mid-plan
isn't investigation), but the plan must read `checkpoint.rs`'s actual
trigger condition and confirm whether it no-ops harmlessly for a
non-`Path`/non-`Command` `PermissionTarget`, or needs an explicit skip
added.

## Testing

- **`aivyx-recall`**: a shared conformance suite run against `Recall` as
  a trait object, so `InMemoryRecall` and `FileRecall` both prove the
  same contract (empty-topic/zero-limit errors, seq monotonicity,
  `get_recent` ordering newest-first, `forget`'s returned count,
  persistence surviving a `FileRecall` re-open) — the same "one behavior
  contract, multiple backends" pattern `aivyx-memory`'s own test suite
  already uses for its substrates.
- **`aivyx-coder`**: tool-level tests against `InMemoryRecall` (no
  filesystem) for argument validation and the `global:`/`project:`
  prefix rewrite; gate-integration tests confirming `memory_write`
  requires confirmation and caches Always-Allow per exact topic,
  `memory_read` auto-allows, and `memory_forget` requires confirmation —
  mirroring the existing gate tests for `write_file`/`read_file`/
  `delete_file`.

## Deferred: `aivyx`'s `aivyx-memory` migration

Not designed or built in this chapter. `aivyx`'s `RedbMemory` would
become a second `Recall` implementor (wrapping its existing
`aivyx_storage::DomainHandle` for `KeyDomain::Memory` encryption, same
as today) alongside `aivyx-recall`'s own `FileRecall`. `aivyx-memory`'s
`Memory` trait, its `Tool` wrappers, capability scopes, audit hooks,
BM25/ANN search, and LRU eviction all stay exactly as they are — only
the lowest storage-substrate layer would eventually point at
`aivyx-recall` instead of reimplementing an equivalent one. That's a
real refactor of ~8,300 lines of mature, tested, many-phases-old
production code, and belongs in its own spec, written and reviewed
inside the `aivyx` repo against its own real test suite, whenever that
work is picked up. Nothing in this chapter's design blocks it — the
`Recall` trait is intentionally the same shape `aivyx-memory`'s own
docs already describe as the substrate-agnostic part.

## Why not embeddings/BM25 here

Retrieval in `aivyx-recall`'s v1 is topic + recency only —
`get_recent(topic, limit)`, no ranking, no index. This matches the
project's existing evidence-based caution about semantic retrieval
(the code-RAG rejection in `docs/HISTORY.md`) and keeps the shared crate
minimal — a search tool (`memory_search`, keyword or eventually
BM25-ranked) is a low-risk, additive v2: `aivyx-memory`'s `bm25.rs` is
already a working, hand-rolled, dependency-free lexical scorer that
could be ported into `aivyx-recall` later as an optional module without
touching the `Recall` trait's core contract.
