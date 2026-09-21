# Parked Specialist-Session Persistence Across Restart Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A parked specialist session survives a process restart: `--resume` loads its `{session_id, member, history}` from disk, and the next `query_specialist` call against that `session_id` transparently rebuilds a live `Agent` from it (lazy rehydration) instead of failing with "unknown session_id".

**Architecture:** `SessionState` gains a new serializable field carrying every open specialist session's `{session_id, member, history}`, written by `Agent::persist()` (reading the lead's existing `specialist_session_pool` handle) and loaded at `--resume` time into a new `dehydrated` map on `SpecialistSessionPool`, alongside its existing live `sessions` map. `query_specialist` checks the dehydrated map on a live-lookup miss and rebuilds via the same `build_specialist_agent` helper `spawn_specialist` already uses; `close_specialist` on a dehydrated-only id just discards the record.

**Tech Stack:** Rust, existing `aivyx-core`/`aivyx-types`/`aivyx` crates, `serde_json` for the on-disk format.

## Global Constraints

- Dehydrated sessions do NOT expire from inactivity (no `last_active` on `PersistedSpecialistSession`) — only live sessions are subject to the existing idle-timeout eviction.
- Dehydrated sessions DO count against `[team] max_concurrent_specialist_sessions` (`has_room()`/`insert_new`'s cap check must read `sessions.len() + dehydrated.len()`).
- `SpecialistSessionPool::snapshot_for_persistence()` MUST union both the live `sessions` map and the still-untouched `dehydrated` map — snapshotting only live sessions would let a dehydrated session silently vanish after exactly one restart instead of surviving indefinitely until resumed or closed.
- A dehydrated session whose `member` no longer exists in the current team roster fails closed on `query_specialist`: discard the record, return a clear error naming the missing member — never silently resume against a stale/wrong roster.
- No change to the TUI/ACP display surfaces (`open_sessions()`, `SpecialistSessionsUpdated`) — dehydrated sessions stay invisible to those panels; only the tool-facing `open_sessions_description()` error-message text changes.
- No change to the live-session idle-timeout mechanism, `close_specialist`'s existing live-session-found code path, or the lead's own `--resume`/session mechanics beyond the one new field.
- File-scoped `rustfmt --edition 2024 <path>` only — NEVER a package-scoped `cargo fmt -p <crate>` command, and NEVER run rustfmt on `crates/aivyx-core/src/agent/mod.rs` or `crates/aivyx-core/src/agent/tests.rs` specifically — that file declares `mod tests;`/`mod types;` as external submodules, and rustfmt on it cascades into unrelated pre-existing reformatting across those files (confirmed empirically earlier in this project's history). Hand-format any new code in those two files to match the surrounding style instead.

---

### Task 1: `PersistedSpecialistSession` type + `SessionState` field + `Agent` history accessors

**Files:**
- Modify: `crates/aivyx-core/src/session.rs`
- Modify: `crates/aivyx-core/src/agent/mod.rs`
- Modify: `crates/aivyx-core/src/agent/tests.rs`

**Interfaces:**
- Produces: `session::PersistedSpecialistSession { session_id: String, member: String, history: Vec<Message> }` (derives `Debug, Clone, Serialize, Deserialize`) — consumed by Task 2 and Task 3.
- Produces: `SessionState::new(history: Vec<Message>, tasks: Vec<Task>, plan_mode_active: bool, specialist_sessions: Vec<PersistedSpecialistSession>) -> Self` — consumed by Task 3 (`Agent::persist()`'s real wiring).
- Produces: `Agent::history_snapshot(&self) -> Vec<Message>` and `Agent::restore_history(&mut self, history: Vec<Message>)` — consumed by Task 2 (`snapshot_for_persistence`) and Task 3 (`query_specialist`'s rehydration path).

- [ ] **Step 1: Add `PersistedSpecialistSession` and the new `SessionState` field**

In `crates/aivyx-core/src/session.rs`, find this exact block:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub version: u32,
    pub history: Vec<Message>,
    pub tasks: Vec<Task>,
    /// Whether Plan mode was active when this session was last persisted.
    /// `--resume` restores it (`Agent::restore`) so quitting mid-review
    /// doesn't silently drop back into Act mode on the next run.
    /// `#[serde(default)]` so a session file written before this field
    /// existed still loads — as `false`, the only behavior possible then.
    #[serde(default)]
    pub plan_mode_active: bool,
}

impl SessionState {
    pub fn new(history: Vec<Message>, tasks: Vec<Task>, plan_mode_active: bool) -> Self {
        Self {
            version: SESSION_VERSION,
            history,
            tasks,
            plan_mode_active,
        }
    }
}
```

Replace it with:

```rust
/// One parked specialist session's persisted state -- just enough to
/// rebuild it: which member it is, and its own conversation history.
/// Unlike `SessionState` (the lead's own persistence format), there's no
/// `last_active`: a dehydrated session doesn't expire from inactivity,
/// since nothing is consuming resources while it sits as inert JSON --
/// only a *live* session (rebuilt via `query_specialist`) is subject to
/// the idle-timeout eviction `SpecialistSessionPool` already has.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSpecialistSession {
    pub session_id: String,
    pub member: String,
    pub history: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub version: u32,
    pub history: Vec<Message>,
    pub tasks: Vec<Task>,
    /// Whether Plan mode was active when this session was last persisted.
    /// `--resume` restores it (`Agent::restore`) so quitting mid-review
    /// doesn't silently drop back into Act mode on the next run.
    /// `#[serde(default)]` so a session file written before this field
    /// existed still loads — as `false`, the only behavior possible then.
    #[serde(default)]
    pub plan_mode_active: bool,
    /// Every specialist session that was open (live or already
    /// dehydrated from an earlier restart) when this file was last
    /// saved. `#[serde(default)]` so a session file written before this
    /// field existed still loads -- as an empty list, the only behavior
    /// possible then. Seeded into `SpecialistSessionPool`'s dehydrated
    /// map at `--resume` time; each entry stays inert until
    /// `query_specialist` rebuilds it, or `close_specialist` discards it
    /// unused.
    #[serde(default)]
    pub specialist_sessions: Vec<PersistedSpecialistSession>,
}

impl SessionState {
    pub fn new(
        history: Vec<Message>,
        tasks: Vec<Task>,
        plan_mode_active: bool,
        specialist_sessions: Vec<PersistedSpecialistSession>,
    ) -> Self {
        Self {
            version: SESSION_VERSION,
            history,
            tasks,
            plan_mode_active,
            specialist_sessions,
        }
    }
}
```

- [ ] **Step 2: Update `session.rs`'s own two `SessionState::new` call sites**

In `crates/aivyx-core/src/session.rs`, find this exact block (inside `round_trips_through_disk`):

```rust
        let state = SessionState::new(
            vec![Message::text(Role::User, "hello")],
            vec![Task {
                id: 1,
                text: "do the thing".to_string(),
                status: TaskStatus::InProgress,
            }],
            true,
        );

        save(&path, &state).unwrap();
        let loaded = load(&path).expect("should load what we just saved");

        assert_eq!(loaded.history.len(), 1);
        assert_eq!(loaded.history[0].text_content(), "hello");
        assert_eq!(loaded.tasks, state.tasks);
        assert!(loaded.plan_mode_active);
    }
```

Replace it with:

```rust
        let state = SessionState::new(
            vec![Message::text(Role::User, "hello")],
            vec![Task {
                id: 1,
                text: "do the thing".to_string(),
                status: TaskStatus::InProgress,
            }],
            true,
            vec![PersistedSpecialistSession {
                session_id: "abc-123".to_string(),
                member: "implementer".to_string(),
                history: vec![Message::text(Role::User, "implement the thing")],
            }],
        );

        save(&path, &state).unwrap();
        let loaded = load(&path).expect("should load what we just saved");

        assert_eq!(loaded.history.len(), 1);
        assert_eq!(loaded.history[0].text_content(), "hello");
        assert_eq!(loaded.tasks, state.tasks);
        assert!(loaded.plan_mode_active);
        assert_eq!(loaded.specialist_sessions.len(), 1);
        assert_eq!(loaded.specialist_sessions[0].session_id, "abc-123");
        assert_eq!(loaded.specialist_sessions[0].member, "implementer");
        assert_eq!(loaded.specialist_sessions[0].history.len(), 1);
        assert_eq!(
            loaded.specialist_sessions[0].history[0].text_content(),
            "implement the thing"
        );
    }

    #[test]
    fn a_session_file_predating_specialist_sessions_still_loads_with_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(
            &path,
            serde_json::json!({ "version": 1, "history": [], "tasks": [] }).to_string(),
        )
        .unwrap();

        let loaded = load(&path).expect("pre-existing-field-free session should still load");

        assert!(loaded.specialist_sessions.is_empty());
    }
```

Then find this exact line (further down, in `saved_file_is_owner_only`):

```rust
        save(&path, &SessionState::new(vec![], vec![], false)).unwrap();
```

Replace it with:

```rust
        save(&path, &SessionState::new(vec![], vec![], false, vec![])).unwrap();
```

- [ ] **Step 3: Run `session.rs`'s tests**

Run: `cargo test -p aivyx-core session:: -- --nocapture`
Expected: `round_trips_through_disk`, the new `a_session_file_predating_specialist_sessions_still_loads_with_none`, and every other existing test in this module pass.

- [ ] **Step 4: Add `Agent::history_snapshot`/`restore_history`**

In `crates/aivyx-core/src/agent/mod.rs`, find this exact block:

```rust
    pub fn restore(&mut self, state: SessionState) {
        self.history = state.history;
        *self.tasks.lock().unwrap() = state.tasks;
        if state.plan_mode_active {
            self.plan_mode.set_active(true);
        }
    }
```

Replace it with:

```rust
    pub fn restore(&mut self, state: SessionState) {
        self.history = state.history;
        *self.tasks.lock().unwrap() = state.tasks;
        if state.plan_mode_active {
            self.plan_mode.set_active(true);
        }
    }

    /// A clone of the current conversation history. Used to persist a
    /// *specialist's* own history (via
    /// `SpecialistSessionPool::snapshot_for_persistence`) -- a different
    /// actor's history than the lead's own `SessionState`-based
    /// persistence this same struct also supports.
    pub fn history_snapshot(&self) -> Vec<Message> {
        self.history.clone()
    }

    /// Replaces the current history wholesale. Used to rehydrate a
    /// dehydrated specialist session's `Agent` (freshly built via
    /// `build_specialist_agent`) with its persisted conversation before
    /// parking it back into the live pool. Unlike `restore()` (the
    /// lead's own `SessionState`-based resume path), this only ever
    /// touches `history` -- a specialist has no tasks or Plan-mode state
    /// of its own to restore.
    pub fn restore_history(&mut self, history: Vec<Message>) {
        self.history = history;
    }
```

- [ ] **Step 5: Add a test for the two new `Agent` methods**

In `crates/aivyx-core/src/agent/tests.rs`, find an existing test that constructs an agent via `build_agent(vec![], ToolRegistry::new(), 5)` (e.g. search for `system_prompt_text_excludes_history`) to confirm the exact call shape, then add nearby:

```rust
#[test]
fn history_snapshot_and_restore_history_round_trip() {
    let (mut agent, _rx, _mock) = build_agent(vec![], ToolRegistry::new(), 5);
    assert!(agent.history_snapshot().is_empty());

    agent.restore_history(vec![Message::text(Role::User, "hello")]);

    let snapshot = agent.history_snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].text_content(), "hello");
}
```

- [ ] **Step 6: Update `agent/tests.rs`'s two `SessionState::new` call sites**

In `crates/aivyx-core/src/agent/tests.rs`, find these exact two lines:

```rust
    agent.restore(crate::session::SessionState::new(vec![], vec![], true));
```

and

```rust
    agent.restore(crate::session::SessionState::new(vec![], vec![], false));
```

Replace them respectively with:

```rust
    agent.restore(crate::session::SessionState::new(vec![], vec![], true, vec![]));
```

and

```rust
    agent.restore(crate::session::SessionState::new(vec![], vec![], false, vec![]));
```

- [ ] **Step 7: Run `aivyx-core`'s tests**

Run: `cargo test -p aivyx-core history_snapshot -- --nocapture` (confirm the new test passes), then `cargo test -p aivyx-core` (confirm the whole crate's suite, including every test touched in Steps 2/6, still passes).
Expected: all pass, 0 failures.

- [ ] **Step 8: Format and commit**

```bash
rustfmt --edition 2024 crates/aivyx-core/src/session.rs
git add crates/aivyx-core/src/session.rs crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/src/agent/tests.rs
git commit -m "feat: PersistedSpecialistSession type + Agent history accessors"
```

Do NOT run `rustfmt` on `crates/aivyx-core/src/agent/mod.rs` or `crates/aivyx-core/src/agent/tests.rs` — see Global Constraints. Hand-format the new code in those two files to match the surrounding 4-space-indent, standard-brace style instead.

---

### Task 2: `SpecialistSessionPool` gains a dehydrated map + cap-counting

**Files:**
- Modify: `crates/aivyx-core/src/specialist_sessions.rs`

**Interfaces:**
- Consumes: `session::PersistedSpecialistSession` (Task 1), `Agent::history_snapshot` (Task 1).
- Produces: `SpecialistSessionPool::seed_dehydrated(&self, sessions: Vec<PersistedSpecialistSession>)`, consumed by Task 3 (`agent_builder.rs`).
- Produces: `SpecialistSessionPool::snapshot_for_persistence(&self) -> Vec<PersistedSpecialistSession>`, consumed by Task 3 (`Agent::persist()`).
- Produces: a private `SpecialistSessionPool::take_dehydrated(&self, id: &str) -> Option<PersistedSpecialistSession>`, consumed by Task 3 (`QuerySpecialistTool`/`CloseSpecialistTool`, same module).

- [ ] **Step 1: Import `PersistedSpecialistSession`**

In `crates/aivyx-core/src/specialist_sessions.rs`, find this exact line:

```rust
use aivyx_types::{ToolDefinition, ToolOutput};
```

Replace it with:

```rust
use aivyx_types::{ToolDefinition, ToolOutput};

use crate::session::PersistedSpecialistSession;
```

- [ ] **Step 2: Add the `dehydrated` field to `SessionPoolState`**

Find this exact block:

```rust
struct SessionPoolState {
    sessions: HashMap<String, ParkedSpecialistSession>,
    max_concurrent: usize,
    idle_timeout: Duration,
}
```

Replace it with:

```rust
struct SessionPoolState {
    sessions: HashMap<String, ParkedSpecialistSession>,
    /// Sessions loaded from a persisted session file at `--resume` time
    /// (via `seed_dehydrated`) that haven't been rebuilt into a live
    /// session yet, plus any still-untouched entries carried forward by
    /// `snapshot_for_persistence` on every later save. No `Agent` exists
    /// for these -- `query_specialist` rebuilds one on first use;
    /// `close_specialist` can also discard one unused.
    dehydrated: HashMap<String, PersistedSpecialistSession>,
    max_concurrent: usize,
    idle_timeout: Duration,
}
```

- [ ] **Step 3: Initialize `dehydrated` in `SpecialistSessionPool::new`**

Find this exact block:

```rust
    pub fn new(max_concurrent: usize, idle_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionPoolState {
                sessions: HashMap::new(),
                max_concurrent,
                idle_timeout,
            })),
        }
    }
```

Replace it with:

```rust
    pub fn new(max_concurrent: usize, idle_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionPoolState {
                sessions: HashMap::new(),
                dehydrated: HashMap::new(),
                max_concurrent,
                idle_timeout,
            })),
        }
    }
```

- [ ] **Step 4: Make `has_room` and `insert_new` count dehydrated sessions against the cap**

Find this exact block:

```rust
    fn has_room(&self) -> bool {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        state.sessions.len() < state.max_concurrent
    }
```

Replace it with:

```rust
    fn has_room(&self) -> bool {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        state.sessions.len() + state.dehydrated.len() < state.max_concurrent
    }
```

Find this exact block:

```rust
    fn insert_new(&self, id: String, session: ParkedSpecialistSession) -> Result<(), usize> {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        if state.sessions.len() >= state.max_concurrent {
            return Err(state.max_concurrent);
        }
        state.sessions.insert(id, session);
        Ok(())
    }
```

Replace it with:

```rust
    fn insert_new(&self, id: String, session: ParkedSpecialistSession) -> Result<(), usize> {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        if state.sessions.len() + state.dehydrated.len() >= state.max_concurrent {
            return Err(state.max_concurrent);
        }
        state.sessions.insert(id, session);
        Ok(())
    }
```

- [ ] **Step 5: Make `open_sessions_description` group live and dehydrated sessions**

Find this exact block:

```rust
    /// A short `"<session_id> (<member>)"` listing of every currently-open
    /// session, comma-separated -- interpolated into `spawn_specialist`'s
    /// cap-exceeded error messages so a model that hits the cap can see
    /// which sessions it could close, mirroring
    /// `delegate_to_specialist.rs`'s own `specialist_names` convention of
    /// giving a model that guessed wrong a recovery path in the same tool
    /// result.
    fn open_sessions_description(&self) -> String {
        self.open_sessions()
            .iter()
            .map(|s| format!("{} ({})", s.session_id, s.member))
            .collect::<Vec<_>>()
            .join(", ")
    }
```

Replace it with:

```rust
    /// A grouped `"N live: <id> (<member>), ...; M dehydrated from a
    /// previous run: <id> (<member>), ..."` listing of every session
    /// counting against the concurrent-session cap -- interpolated into
    /// `spawn_specialist`'s cap-exceeded error messages so a model that
    /// hits the cap can see exactly which `session_id` to
    /// `close_specialist`, including a dehydrated session it hasn't
    /// touched yet this run. Either group is omitted entirely when
    /// empty. Mirrors `delegate_to_specialist.rs`'s own `specialist_names`
    /// convention of giving a model that hit a limit a recovery path in
    /// the same tool result.
    fn open_sessions_description(&self) -> String {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        let mut live: Vec<String> = state
            .sessions
            .iter()
            .map(|(id, s)| format!("{id} ({})", s.member))
            .collect();
        live.sort();
        let mut dehydrated: Vec<String> = state
            .dehydrated
            .iter()
            .map(|(id, s)| format!("{id} ({})", s.member))
            .collect();
        dehydrated.sort();
        drop(state);

        let mut parts = Vec::new();
        if !live.is_empty() {
            parts.push(format!("{} live: {}", live.len(), live.join(", ")));
        }
        if !dehydrated.is_empty() {
            parts.push(format!(
                "{} dehydrated from a previous run: {}",
                dehydrated.len(),
                dehydrated.join(", ")
            ));
        }
        parts.join("; ")
    }
```

- [ ] **Step 6: Add `seed_dehydrated`, `take_dehydrated`, and `snapshot_for_persistence`**

Find this exact block (the end of the `SpecialistSessionPool` `impl` block):

```rust
    /// Removes and returns a session so its turn can run WITHOUT holding
    /// the pool's lock -- the whole point of this method existing
    /// instead of a borrow-returning accessor. `None` if the id doesn't
    /// exist or has gone stale.
    fn take(&self, id: &str) -> Option<ParkedSpecialistSession> {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        state.sessions.remove(id)
    }
```

Replace it with:

```rust
    /// Removes and returns a session so its turn can run WITHOUT holding
    /// the pool's lock -- the whole point of this method existing
    /// instead of a borrow-returning accessor. `None` if the id doesn't
    /// exist or has gone stale.
    fn take(&self, id: &str) -> Option<ParkedSpecialistSession> {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        state.sessions.remove(id)
    }

    /// Loads a previously-persisted set of specialist sessions into the
    /// dehydrated map, called once at `--resume` time (only when a
    /// resumable session was actually found) before any tool call runs.
    /// Each entry stays inert -- no `Agent` is built -- until
    /// `query_specialist` rehydrates it on first use, or
    /// `close_specialist` discards it unused.
    pub fn seed_dehydrated(&self, sessions: Vec<PersistedSpecialistSession>) {
        let mut state = self.inner.lock().unwrap();
        for session in sessions {
            state.dehydrated.insert(session.session_id.clone(), session);
        }
    }

    /// Removes and returns a dehydrated session record so it can be
    /// rebuilt into a live one, mirroring `take`'s remove-and-return
    /// shape.
    fn take_dehydrated(&self, id: &str) -> Option<PersistedSpecialistSession> {
        self.inner.lock().unwrap().dehydrated.remove(id)
    }

    /// A snapshot of every session that should survive a restart --
    /// every currently-live session's `(id, member, history)` PLUS every
    /// still-dehydrated entry, carried forward unchanged. Both maps MUST
    /// be unioned: snapshotting only live sessions would let a
    /// dehydrated session survive exactly one restart and then silently
    /// vanish on the very next save, since nothing else carries it
    /// forward -- unioning both means a dehydrated session survives
    /// indefinitely, across any number of restarts, until it's either
    /// resumed (moves into the live map from then on) or explicitly
    /// closed. Called from `Agent::persist()`.
    pub fn snapshot_for_persistence(&self) -> Vec<PersistedSpecialistSession> {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        let mut out: Vec<PersistedSpecialistSession> =
            state.dehydrated.values().cloned().collect();
        out.extend(
            state
                .sessions
                .iter()
                .map(|(id, session)| PersistedSpecialistSession {
                    session_id: id.clone(),
                    member: session.member.clone(),
                    history: session.agent.history_snapshot(),
                }),
        );
        out
    }
```

- [ ] **Step 7: Add tests**

In `crates/aivyx-core/src/specialist_sessions.rs`'s existing `#[cfg(test)] mod tests` block, find the `close_all_removes_every_open_session` test (added in an earlier phase of this same feature area) to confirm the exact shape of `config(...)`/`simple_team()`/`exec_ctx(...)`/`MockBackend`/`text_response(...)` already available in this module, then add:

```rust
#[tokio::test]
async fn snapshot_for_persistence_includes_a_live_sessions_current_history() {
    let llm = Arc::new(MockBackend::new(vec![text_response("hello")]));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
    let cfg = config(llm, tx, simple_team(), pool.clone());
    let spawn_tool = SpawnSpecialistTool::new(cfg);
    let ctx = exec_ctx(std::path::Path::new("."));

    spawn_tool
        .execute(
            serde_json::json!({ "member": "implementer", "task": "do something" }),
            &ctx,
        )
        .await
        .unwrap();

    let snapshot = pool.snapshot_for_persistence();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].member, "implementer");
    assert!(
        !snapshot[0].history.is_empty(),
        "a live session's snapshot must carry its real conversation history"
    );
}

#[tokio::test]
async fn snapshot_for_persistence_carries_dehydrated_sessions_forward_unchanged() {
    let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
    let persisted = PersistedSpecialistSession {
        session_id: "old-session".to_string(),
        member: "implementer".to_string(),
        history: vec![aivyx_types::Message::text(
            aivyx_types::Role::User,
            "from a previous run",
        )],
    };
    pool.seed_dehydrated(vec![persisted]);

    let snapshot = pool.snapshot_for_persistence();

    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].session_id, "old-session");
    assert_eq!(snapshot[0].member, "implementer");
    assert_eq!(snapshot[0].history.len(), 1);
}

#[tokio::test]
async fn dehydrated_sessions_count_against_the_concurrent_session_cap() {
    let llm = Arc::new(MockBackend::new(vec![text_response("hello")]));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let pool = SpecialistSessionPool::new(1, Duration::from_secs(600));
    pool.seed_dehydrated(vec![PersistedSpecialistSession {
        session_id: "old-session".to_string(),
        member: "implementer".to_string(),
        history: vec![],
    }]);
    let cfg = config(llm, tx, simple_team(), pool.clone());
    let spawn_tool = SpawnSpecialistTool::new(cfg);
    let ctx = exec_ctx(std::path::Path::new("."));

    let output = spawn_tool
        .execute(
            serde_json::json!({ "member": "implementer", "task": "do something else" }),
            &ctx,
        )
        .await
        .unwrap();

    match output {
        ToolOutput::Error(msg) => {
            assert!(
                msg.contains("dehydrated from a previous run"),
                "cap-exceeded message should mention the dehydrated session blocking room: {msg}"
            );
        }
        ToolOutput::Ok(_) => panic!(
            "a dehydrated session should count against max_concurrent=1, blocking this spawn"
        ),
    }
}
```

(Check the exact `config`/`simple_team`/`exec_ctx`/`text_response`/`MockBackend` signatures already in this file before finalizing these tests' exact call shapes — mirror whatever the file's own existing tests already do, adapting only what's shown above if the real helpers differ.)

- [ ] **Step 8: Run the tests**

Run: `cargo test -p aivyx-core specialist_sessions:: -- --nocapture`
Expected: all tests in this module pass, including the three new ones.

- [ ] **Step 9: Format and commit**

```bash
rustfmt --edition 2024 crates/aivyx-core/src/specialist_sessions.rs
git add crates/aivyx-core/src/specialist_sessions.rs
git commit -m "feat: SpecialistSessionPool gains a dehydrated map for persisted sessions"
```

---

### Task 3: Rehydration flow + `persist()`/`agent_builder.rs` wiring

**Files:**
- Modify: `crates/aivyx-core/src/specialist_sessions.rs`
- Modify: `crates/aivyx-core/src/agent/mod.rs`
- Modify: `crates/aivyx/src/agent_builder.rs`

**Interfaces:**
- Consumes: `SpecialistSessionPool::seed_dehydrated`, `snapshot_for_persistence`, `take_dehydrated` (Task 2); `Agent::history_snapshot`/`restore_history` (Task 1); `SessionState::new`'s 4-arg signature (Task 1).

- [ ] **Step 1: Wire the real specialist-session snapshot into `Agent::persist()`**

In `crates/aivyx-core/src/agent/mod.rs`, find this exact block:

```rust
    fn persist(&self) {
        let Some(path) = &self.session_path else {
            return;
        };
        let tasks = self.tasks.lock().unwrap().clone();
        let state = SessionState::new(self.history.clone(), tasks, self.plan_mode.active());
        if let Err(err) = session::save(path, &state) {
            tracing::warn!(error = %err, "failed to persist session");
        }
    }
```

Replace it with:

```rust
    fn persist(&self) {
        let Some(path) = &self.session_path else {
            return;
        };
        let tasks = self.tasks.lock().unwrap().clone();
        let specialist_sessions = self
            .specialist_session_pool
            .as_ref()
            .map(SpecialistSessionPool::snapshot_for_persistence)
            .unwrap_or_default();
        let state = SessionState::new(
            self.history.clone(),
            tasks,
            self.plan_mode.active(),
            specialist_sessions,
        );
        if let Err(err) = session::save(path, &state) {
            tracing::warn!(error = %err, "failed to persist session");
        }
    }
```

- [ ] **Step 2: Run `aivyx-core`'s tests to confirm `persist()` still compiles and behaves**

Run: `cargo test -p aivyx-core clear_conversation -- --nocapture` (these tests exercise `persist()` indirectly via `clear_conversation`) and `cargo build -p aivyx-core`.
Expected: compiles cleanly, tests pass.

- [ ] **Step 3: Add the rehydration branch to `QuerySpecialistTool::execute`**

In `crates/aivyx-core/src/specialist_sessions.rs`, find this exact block:

```rust
    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: QuerySpecialistArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let Some(mut session) = self.config.pool.take(&args.session_id) else {
            return Ok(ToolOutput::Error(format!(
                "unknown session_id: {:?} -- it may not exist, may already be closed, or may \
                have expired from inactivity; call spawn_specialist to start a new one",
                args.session_id
            )));
        };

        let output = run_bounded_exchange(
            &mut session.agent,
            args.message,
            ctx,
            &self.config,
            &session.accumulated,
            &session.barrier_tx,
        )
        .await;

        self.config.pool.put_back(args.session_id, session);
        Ok(output)
    }
}
```

Replace it with:

```rust
    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: QuerySpecialistArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let mut session = if let Some(session) = self.config.pool.take(&args.session_id) {
            session
        } else if let Some(persisted) = self.config.pool.take_dehydrated(&args.session_id) {
            let Some(member) = self
                .config
                .team
                .members
                .iter()
                .find(|m| m.name == persisted.member)
            else {
                return Ok(ToolOutput::Error(format!(
                    "cannot resume session_id {:?}: its specialist {:?} no longer exists in \
                    the current team roster -- the session has been discarded; call \
                    spawn_specialist with a valid member instead",
                    args.session_id, persisted.member
                )));
            };
            let (mut agent, forward_task, accumulated, barrier_tx) =
                build_specialist_agent(member, &self.config, &ctx.cwd);
            agent.restore_history(persisted.history);
            ParkedSpecialistSession {
                agent,
                member: member.name.clone(),
                forward_task,
                accumulated,
                barrier_tx,
                last_active: Instant::now(),
            }
        } else {
            return Ok(ToolOutput::Error(format!(
                "unknown session_id: {:?} -- it may not exist, may already be closed, or may \
                have expired from inactivity; call spawn_specialist to start a new one",
                args.session_id
            )));
        };

        let output = run_bounded_exchange(
            &mut session.agent,
            args.message,
            ctx,
            &self.config,
            &session.accumulated,
            &session.barrier_tx,
        )
        .await;

        self.config.pool.put_back(args.session_id, session);
        Ok(output)
    }
}
```

- [ ] **Step 4: Add the dehydrated-close branch to `CloseSpecialistTool::execute`**

In `crates/aivyx-core/src/specialist_sessions.rs`, find this exact block:

```rust
    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: CloseSpecialistArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let Some(session) = self.config.pool.take(&args.session_id) else {
            return Ok(ToolOutput::Error(format!(
                "unknown session_id: {:?} -- it may not exist or may already be closed",
                args.session_id
            )));
        };
        let member = session.member.clone();
        drop(session.agent);
        let _ = session.forward_task.await;

        let _ = self
            .config
            .events_tx
            .send(AgentEvent::SpecialistSessionsUpdated(
                self.config.pool.open_sessions(),
            ));

        Ok(ToolOutput::Ok(format!(
            "specialist session {:?} closed ({member})",
            args.session_id
        )))
    }
```

Replace it with:

```rust
    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: CloseSpecialistArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        if let Some(session) = self.config.pool.take(&args.session_id) {
            let member = session.member.clone();
            drop(session.agent);
            let _ = session.forward_task.await;

            let _ = self
                .config
                .events_tx
                .send(AgentEvent::SpecialistSessionsUpdated(
                    self.config.pool.open_sessions(),
                ));

            return Ok(ToolOutput::Ok(format!(
                "specialist session {:?} closed ({member})",
                args.session_id
            )));
        }

        if let Some(persisted) = self.config.pool.take_dehydrated(&args.session_id) {
            return Ok(ToolOutput::Ok(format!(
                "specialist session {:?} closed ({}) -- it was dehydrated from a previous \
                run and had not been resumed",
                args.session_id, persisted.member
            )));
        }

        Ok(ToolOutput::Error(format!(
            "unknown session_id: {:?} -- it may not exist or may already be closed",
            args.session_id
        )))
    }
```

- [ ] **Step 5: Add tests**

In `crates/aivyx-core/src/specialist_sessions.rs`'s test module, add:

```rust
#[tokio::test]
async fn query_specialist_rehydrates_a_dehydrated_session_and_completes_the_query() {
    let llm = Arc::new(MockBackend::new(vec![text_response("hi again")]));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
    pool.seed_dehydrated(vec![PersistedSpecialistSession {
        session_id: "old-session".to_string(),
        member: "implementer".to_string(),
        history: vec![aivyx_types::Message::text(
            aivyx_types::Role::User,
            "earlier task from a previous run",
        )],
    }]);
    let cfg = config(Arc::clone(&llm), tx, simple_team(), pool.clone());
    let query_tool = QuerySpecialistTool::new(cfg);
    let ctx = exec_ctx(std::path::Path::new("."));

    let output = query_tool
        .execute(
            serde_json::json!({ "session_id": "old-session", "message": "follow up" }),
            &ctx,
        )
        .await
        .unwrap();

    assert!(matches!(output, ToolOutput::Ok(_)));
    assert_eq!(
        pool.open_sessions().len(),
        1,
        "rehydration should move the session into the live pool"
    );
    let received = llm.received.lock().unwrap();
    let last_request = received.last().expect("the mock should have received a request");
    assert!(
        last_request
            .messages
            .iter()
            .any(|m| m.text_content().contains("earlier task from a previous run")),
        "the rebuilt agent's request should include the restored history, not just the new \
        follow-up message"
    );
}

#[tokio::test]
async fn query_specialist_fails_closed_when_the_dehydrated_members_no_longer_in_the_roster() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
    pool.seed_dehydrated(vec![PersistedSpecialistSession {
        session_id: "old-session".to_string(),
        member: "no-longer-on-the-roster".to_string(),
        history: vec![],
    }]);
    let llm = Arc::new(MockBackend::new(vec![]));
    let cfg = config(llm, tx, simple_team(), pool.clone());
    let query_tool = QuerySpecialistTool::new(cfg);
    let ctx = exec_ctx(std::path::Path::new("."));

    let output = query_tool
        .execute(
            serde_json::json!({ "session_id": "old-session", "message": "follow up" }),
            &ctx,
        )
        .await
        .unwrap();

    match output {
        ToolOutput::Error(msg) => {
            assert!(msg.contains("no-longer-on-the-roster"));
        }
        ToolOutput::Ok(_) => panic!("must fail closed when the member no longer exists"),
    }
    assert!(
        pool.open_sessions().is_empty(),
        "the stale dehydrated record must be discarded, not left around"
    );
}

#[tokio::test]
async fn close_specialist_discards_a_dehydrated_only_session_without_building_an_agent() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
    pool.seed_dehydrated(vec![PersistedSpecialistSession {
        session_id: "old-session".to_string(),
        member: "implementer".to_string(),
        history: vec![],
    }]);
    let llm = Arc::new(MockBackend::new(vec![]));
    let cfg = config(llm, tx, simple_team(), pool.clone());
    let close_tool = CloseSpecialistTool::new(cfg);
    let ctx = exec_ctx(std::path::Path::new("."));

    let output = close_tool
        .execute(serde_json::json!({ "session_id": "old-session" }), &ctx)
        .await
        .unwrap();

    assert!(matches!(output, ToolOutput::Ok(_)));
    assert_eq!(pool.snapshot_for_persistence().len(), 0);
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p aivyx-core specialist_sessions:: -- --nocapture`
Expected: all tests pass, including the three new ones from this step and the three from Task 2.

- [ ] **Step 7: Format and commit**

```bash
rustfmt --edition 2024 crates/aivyx-core/src/specialist_sessions.rs
git add crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/src/specialist_sessions.rs
git commit -m "feat: query_specialist rehydrates dehydrated sessions on demand"
```

Do NOT run `rustfmt` on `crates/aivyx-core/src/agent/mod.rs` (see Global Constraints) — hand-format the `persist()` change to match the surrounding style.

- [ ] **Step 8: Seed the pool from a resumed session in `agent_builder.rs`**

In `crates/aivyx/src/agent_builder.rs`, find this exact block:

```rust
            if let Some(state) = &restored {
                agent.restore(state.clone());
            }
            agent.set_session_path(path);
            restored
```

Replace it with:

```rust
            if let Some(state) = &restored {
                agent.restore(state.clone());
                if let Some(pool) = &specialist_session_pool {
                    pool.seed_dehydrated(state.specialist_sessions.clone());
                }
            }
            agent.set_session_path(path);
            restored
```

- [ ] **Step 9: Verify `aivyx` compiles**

Run: `cargo check -p aivyx`
Expected: compiles cleanly.

- [ ] **Step 10: Build/test/lint the full workspace**

Run:
```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```
Expected: all clean, zero failures, zero warnings.

- [ ] **Step 11: Commit**

```bash
git add crates/aivyx/src/agent_builder.rs
git commit -m "feat: seed dehydrated specialist sessions from a resumed session file"
```

---

## Self-Review Notes

**Spec coverage:** Decision 1 (`SessionState`/`PersistedSpecialistSession`) → Task 1. Decision 2 (`Agent::history_snapshot`/`restore_history`, `persist()` wiring) → Task 1 + Task 3 Step 1. Decision 3 (`SpecialistSessionPool`'s `dehydrated` map, `seed_dehydrated`, `snapshot_for_persistence` with its required union, `take_dehydrated`) → Task 2. Decision 4 (dehydrated sessions count against the cap, grouped error message) → Task 2 Steps 4-5. Decision 5 (`query_specialist`'s rehydration flow, fail-closed on a missing roster member) → Task 3 Step 3. Decision 6 (`close_specialist` on a dehydrated-only id) → Task 3 Step 4. "What this spec does not decide" items are all genuinely untouched: no TUI/ACP display change, no idle-timeout change for dehydrated sessions, no change to `close_specialist`'s live-found path's own logic, no attempt to preserve sessions across a `[team] enabled` toggle-off interval, no work on the other two remaining Nonagon gaps.

**Global Constraints deviation:** none — every constraint is directly implemented by name in the tasks above (cap-counting in Task 2 Step 4, union-both-maps in Task 2 Step 6's doc comment and logic, fail-closed roster check in Task 3 Step 3, no TUI/ACP change anywhere, only file-scoped `rustfmt`, and `agent/mod.rs`/`agent/tests.rs` are never passed to `rustfmt` in any task).

**Placeholder scan:** no TBD/TODO; every step shows complete, real code. Task 2 Step 7 and Task 3 Step 5 are deliberately investigative about *exact existing test-module helper signatures* (matching this project's own established plan-writing precedent for details a plan author can't fully re-verify character-for-character without re-reading the whole file inline) — what to test is fully specified.

**Type/interface consistency check:** `PersistedSpecialistSession { session_id, member, history }` (Task 1) is constructed identically in every later task (Task 2's tests, Task 3's tests, Task 3's `agent_builder.rs` wiring via `state.specialist_sessions.clone()`). `SpecialistSessionPool::seed_dehydrated(&self, sessions: Vec<PersistedSpecialistSession>)`/`snapshot_for_persistence(&self) -> Vec<PersistedSpecialistSession>`/`take_dehydrated(&self, id: &str) -> Option<PersistedSpecialistSession>` (Task 2) are called with matching signatures at every Task 3 use site. `Agent::history_snapshot(&self) -> Vec<Message>`/`restore_history(&mut self, history: Vec<Message>)` (Task 1) match their Task 2 (`snapshot_for_persistence`) and Task 3 (`query_specialist`'s rehydration) call sites exactly. `SessionState::new`'s new 4-arg signature (Task 1) matches its Task 3 Step 1 call site.

**Known, deliberately accepted gap** (documented here rather than engineered around, matching this project's established precedent from the `/clear` fix's own final review): Task 3 Step 8's two-line `agent_builder.rs` wiring has no dedicated automated test — there's no existing test infrastructure exercising `build_agent`'s full `--resume` flow end-to-end, and building one from scratch for this one call site would be disproportionate to the change. Task 2/3's own unit tests directly cover `seed_dehydrated`/`snapshot_for_persistence`/the rehydration flow at the `SpecialistSessionPool`/tool level, which is where the real logic lives; the `agent_builder.rs` wiring itself is a straight pass-through, verified by `cargo check -p aivyx` (Task 3 Step 9) and manual reasoning during planning (documented in the spec's own Grounding section).
