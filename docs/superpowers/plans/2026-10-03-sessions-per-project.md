# Several Sessions Per Project Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** aivyx-coder keeps up to 20 conversations per project instead of one overwritten slot, lists them with
`/sessions`, switches with `/resume N` (or `--resume[=N]` at startup), and `/clear` starts a new one instead of
wiping the old.

**Architecture:**
- **Storage.** A `SessionStore` (in `crates/aivyx-core/src/session.rs`) owns one directory per project, holding
  one JSON file per conversation, each with a `SessionMeta` header.
- **Agent.** It persists through a `SessionTarget`:
  - `File(path)`, which keeps today's single-file behaviour for existing tests;
  - `Store { store, id }`, used in production. The id stays `None` until the first successful persist, so a failed
    first turn never creates a file (audit-2 B1).
- **Commands.** `/sessions` and `/resume N` are agent commands intercepted in `run_turn`. A switch emits
  `AgentEvent::SessionSwitched`, and the TUI rebuilds its transcript from it.

**Tech Stack:** Rust 2024, serde_json, chrono (no `alloc` formatting; names via tables), clap 4 derive, ratatui.

## Global Constraints

- Repo `/home/julian/Projects/Rust/aivyx-coder`. Read `CLAUDE.md` first.
- Spec: `docs/superpowers/specs/2026-10-02-sessions-per-project-design.md`.
- **Deliberate deviations from the spec:**
  - Saves become atomic (temp file plus rename). The spec says "as today", but today's `session::save` truncates
    in place.
  - ACP keeps persisting, as it does today, but into the new store (it builds its agent through the same
    `agent_builder`). Writing the legacy single file would fight the migration. ACP refuses `/resume` because it
    can't redraw. MCP sessions don't persist (unchanged).
- Layout:
  - `<state>/sessions/<project-key>/` (dir mode 0700), with `<project-key>` exactly today's key (the file stem
    `session_file_path` produces);
  - one file per conversation: `<created_unix_ms>-<8 hex>.json`, mode 0600, written to a temp file in the same
    dir and then `rename`d;
  - the legacy single file `<state>/sessions/<project-key>.json`.
- Keep the newest **20** by `updated_unix` (`SESSIONS_KEPT = 20`). `first_user_text` keeps at most **60** chars,
  with "…" appended when cut.
- Exact user-facing strings:
  - Listing header: `Conversations for this project (newest first — /resume N to switch):`
  - Listing row: `{n}  {when} · {turns} turn(s) · "{first_user_text}"`, plus `  (current)` on the current one.
    `turn` is singular for 1.
  - `{when}`, in local time: `today HH:MM`, `yesterday HH:MM`, otherwise `Mon 29 Sep HH:MM`.
  - None saved: `No saved conversations for this project yet.`
  - `/resume` with no argument or a non-number: `Use /resume N — /sessions lists them.`
  - Out of range, both `/resume N` and `--resume=N`:
    `There are only K saved conversations for this project (see /sessions).`
  - After a switch: `Resumed conversation N (T turns)` (`turn` singular for 1).
  - `/resume` where switching isn't supported (ACP, no store): `/resume isn't available here.`
  - `/sessions` with no store: `Sessions aren't saved here.`
  - Busy (TUI only): `Wait for the reply to finish (or press Ctrl+C) first.`
  - `/clear` help line: `Start a new conversation (the old one stays in /sessions)`
  - TUI notice after `/clear`: `New conversation — the previous one is in /sessions.`
- Commits: `git commit -s`, message ending with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  Branch `feat/sessions-per-project`.
- TDD: show RED before GREEN for every behaviour change.
- Verify at the end of each task:
  - `cargo test --workspace`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `CARGO_TARGET_DIR=target/rust199 ~/.cargo/bin/cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`
  - Never put build output in `/tmp`.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/aivyx-core/src/session.rs` | `SessionMeta`, `SessionState.meta`, atomic `save`, `SessionStore` (list/load/save/prune/migrate/new id), `project_key`. |
| `crates/aivyx-core/src/session_list.rs` (new) | Pure formatting: `when_label`, `sessions_listing`. |
| `crates/aivyx-core/src/agent/mod.rs` | `SessionTarget`, store-backed `persist`, `/clear` starting a new conversation, `switch_to`. |
| `crates/aivyx-core/src/agent/session_commands.rs` (new) | `/sessions`, `/resume N` parse + run. |
| `crates/aivyx-core/src/agent/types.rs` | `AgentEvent::SessionSwitched { history, tasks }`. |
| `crates/aivyx-core/src/commands.rs` | `/sessions`, `/resume` entries; new `/clear` description. |
| `crates/aivyx/src/main.rs`, `agent_builder.rs` | `--resume[=N]`, store wiring + migration, enabling switching for the TUI. |
| `crates/aivyx-tui/src/app.rs` | `SessionSwitched` rebuild, busy refusal for `/resume`, `/clear` notice. |
| `crates/aivyx-acp/src/translate.rs` | `SessionSwitched` → `None`. |
| `README.md`, `CLAUDE.md` | Docs. |

---

### Task 1: The session store

**Files:**
- Modify: `crates/aivyx-core/src/session.rs`
- Test: its `#[cfg(test)] mod tests`

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub created_unix: i64,
    pub updated_unix: i64,
    pub first_user_text: String,
    pub turns: usize,
}
// SessionState gains: #[serde(default)] pub meta: SessionMeta,
pub const SESSIONS_KEPT: usize = 20;
pub fn project_key(cwd: &Path) -> String;               // today's key, e.g. "proj-0123456789abcdef"
pub fn sessions_root() -> Option<PathBuf>;              // <state>/sessions
pub fn preview_text(text: &str) -> String;              // ≤60 chars + "…", newlines → spaces, leading "(note)\n\n" stripped
pub fn count_turns(history: &[Message]) -> usize;       // number of Role::User messages
#[derive(Debug, Clone)]
pub struct SessionStore { dir: PathBuf }
impl SessionStore {
    pub fn new(dir: PathBuf) -> Self;
    pub fn for_project(cwd: &Path) -> Option<Self>;     // sessions_root()/<project_key>
    pub fn dir(&self) -> &Path;
    pub fn new_id(now_ms: i64) -> String;               // "<now_ms>-<8 hex>"
    pub fn path_for(&self, id: &str) -> PathBuf;        // dir/<id>.json
    pub fn list(&self) -> Vec<SessionMeta>;             // newest updated first; unreadable files skipped
    pub fn load(&self, id: &str) -> Option<SessionState>;
    pub fn save(&self, state: &SessionState) -> std::io::Result<()>; // path from state.meta.id
    pub fn prune(&self, keep: usize);                   // delete all but the newest `keep`
    pub fn migrate_legacy(&self, legacy: &Path) -> std::io::Result<bool>;
}
```

**Behaviour:**
- **`save` (both the free fn and the store's).** Write the JSON to `<path>.tmp-<pid>` with mode 0600, `sync_all`,
  then `rename` over the path. Create the parent dir with mode 0700, as today.
- **`list`.** Reads every `*.json` in the dir (not `*.tmp-*`) with `load`. An entry whose `meta.id` is empty, as
  in a hand-copied old file, takes its id from the file stem. Sort by `updated_unix` descending, ties by id
  descending.
- **`migrate_legacy(legacy)`.**
  - Does nothing (`Ok(false)`) unless `legacy` is a regular file AND `self.dir` doesn't exist.
  - It loads the legacy file. An unparseable one is still moved, but its meta is derived from the mtime only.
  - The meta is:
    - `created_unix` and `updated_unix` = the file's mtime (seconds);
    - `id` = `new_id(mtime_ms)`;
    - `first_user_text` = `preview_text` of the first `Role::User` message;
    - `turns` = `count_turns`.
  - It saves into the store, then removes the legacy file and returns `Ok(true)`.
  - An unparseable legacy file is renamed into the dir as `<id>.json` unchanged, so nothing is lost.
- **`prune(keep)`.** Deletes the files of every listed entry past index `keep`.
- **`preview_text`.**
  - Strips a leading note block (the text starts with `(` and contains `)\n\n`): drop everything up to and
    including the first `)\n\n`.
  - Collapses whitespace runs, newlines included, to single spaces, then trims.
  - Keeps the first 60 chars, appending `…` if cut.
- **`new_id(now_ms)`.** `format!("{now_ms}-{:08x}", (fnv1a(format!("{now_ms}-{}-{:?}", std::process::id(),
  std::time::Instant::now()).as_bytes()) & 0xffff_ffff) as u32)`.
- **`session_file_path`.** Stays: it is the legacy path, used by migration. Re-express it via `project_key` and
  `sessions_root` so the key logic exists once.

- [ ] **Step 1: Write the failing tests**

```rust
    fn meta_state(id: &str, updated: i64, first: &str) -> SessionState {
        let mut s = SessionState::new(vec![Message::text(Role::User, first)], vec![], false, vec![]);
        s.meta = SessionMeta {
            id: id.into(),
            created_unix: updated,
            updated_unix: updated,
            first_user_text: first.into(),
            turns: 1,
        };
        s
    }

    #[test]
    fn store_lists_newest_first_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().join("proj"));
        store.save(&meta_state("1000-aaaaaaaa", 10, "old")).unwrap();
        store.save(&meta_state("2000-bbbbbbbb", 30, "newest")).unwrap();
        store.save(&meta_state("1500-cccccccc", 20, "middle")).unwrap();
        let ids: Vec<String> = store.list().into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["2000-bbbbbbbb", "1500-cccccccc", "1000-aaaaaaaa"]);
        let loaded = store.load("1500-cccccccc").unwrap();
        assert_eq!(loaded.history[0].text_content(), "middle");
    }

    #[cfg(unix)]
    #[test]
    fn store_files_are_0600_in_a_0700_dir_and_leave_no_temp_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().join("proj"));
        store.save(&meta_state("1-aaaaaaaa", 1, "x")).unwrap();
        store.save(&meta_state("1-aaaaaaaa", 2, "x")).unwrap();
        let dir_mode = std::fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777;
        let file_mode = std::fs::metadata(store.path_for("1-aaaaaaaa")).unwrap().permissions().mode() & 0o777;
        assert_eq!((dir_mode, file_mode), (0o700, 0o600));
        let names: Vec<String> = std::fs::read_dir(store.dir()).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["1-aaaaaaaa.json".to_string()]);
    }

    #[test]
    fn prune_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().join("proj"));
        for i in 0..25 {
            store.save(&meta_state(&format!("{i}-00000000"), i, "t")).unwrap();
        }
        store.prune(SESSIONS_KEPT);
        let list = store.list();
        assert_eq!(list.len(), 20);
        assert_eq!(list[0].updated_unix, 24);
        assert_eq!(list[19].updated_unix, 5);
    }

    #[test]
    fn migrate_moves_the_legacy_file_in_and_derives_its_header() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("proj-0000000000000001.json");
        let old = SessionState::new(
            vec![
                Message::text(Role::User, "(You undid the last turn.)\n\nthe tests in\n  test_stats.py fail"),
                Message::text(Role::Assistant, "ok"),
                Message::text(Role::User, "again"),
            ],
            vec![],
            false,
            vec![],
        );
        save(&legacy, &old).unwrap();
        let store = SessionStore::new(dir.path().join("proj-0000000000000001"));
        assert!(store.migrate_legacy(&legacy).unwrap());
        assert!(!legacy.exists());
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].first_user_text, "the tests in test_stats.py fail");
        assert_eq!(list[0].turns, 2);
        assert!(list[0].updated_unix > 0);
        assert_eq!(store.load(&list[0].id).unwrap().history.len(), 3);
        // Second run: the directory exists, nothing happens.
        assert!(!store.migrate_legacy(&legacy).unwrap());
    }

    #[test]
    fn migrate_keeps_an_unparseable_legacy_file() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("p.json");
        std::fs::write(&legacy, "{not json").unwrap();
        let store = SessionStore::new(dir.path().join("p"));
        assert!(store.migrate_legacy(&legacy).unwrap());
        assert!(!legacy.exists());
        let kept: Vec<_> = std::fs::read_dir(store.dir()).unwrap().collect();
        assert_eq!(kept.len(), 1, "the bytes are kept, even if unlistable");
    }

    #[test]
    fn old_files_load_with_a_default_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        std::fs::write(&path, r#"{"version":1,"history":[],"tasks":[]}"#).unwrap();
        assert_eq!(load(&path).unwrap().meta, SessionMeta::default());
    }

    #[test]
    fn preview_text_strips_notes_collapses_space_and_caps_at_60() {
        assert_eq!(preview_text("(note one) (note two)\n\nfix  the\nbug"), "fix the bug");
        let long = "x".repeat(70);
        assert_eq!(preview_text(&long), format!("{}…", "x".repeat(60)));
        assert_eq!(preview_text("(not a note) just text"), "(not a note) just text");
    }

    #[test]
    fn new_ids_sort_by_time_and_differ() {
        let a = SessionStore::new_id(1000);
        let b = SessionStore::new_id(1000);
        assert!(a.starts_with("1000-") && a.len() == "1000-".len() + 8);
        assert_ne!(a, b);
    }
```

- [ ] **Step 2: Run them to verify they fail.** `cargo test -p aivyx-core session::` gives compile errors.

- [ ] **Step 3: Implement the interfaces and behaviour above.** Keep the free `load`/`save` functions; existing
  callers use them. `SessionStore::save` delegates to the free `save(&self.path_for(&state.meta.id), state)`.
  Update the module doc comment for the new layout.

- [ ] **Step 4: Run all `session::` tests plus the Global Constraints verification.**

- [ ] **Step 5: Commit** (`session store: one file per conversation, atomic writes, legacy migration`).

---

### Task 2: The agent persists into the store

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs`
- Test: append to `crates/aivyx-core/src/agent/tests.rs`

**Interfaces:**
- Consumes (Task 1): `SessionStore`, `SessionMeta`, `SESSIONS_KEPT`, `preview_text`, `count_turns`.
- Produces:
  - `enum SessionTarget { File(PathBuf), Store { store: SessionStore, id: Option<String>, created_unix: i64 } }`
    (private to the agent module);
  - `Agent::set_session_store(&mut self, store: SessionStore)`;
  - `Agent::session_store(&self) -> Option<&SessionStore>`;
  - `Agent::current_session_id(&self) -> Option<&str>`;
  - `Agent::restore_session(&mut self, state: SessionState)`: `restore` plus adopting `state.meta` as the current
    id and first text;
  - `pub(super) fn now_unix_ms() -> i64`.

**Behaviour:**
- **The field.** `session_path: Option<PathBuf>` becomes `session_target: Option<SessionTarget>`.
  - `set_session_path(path)` sets `File(path)`, unchanged for existing tests.
  - `set_session_store(store)` sets `Store { store, id: None, created_unix: 0 }`.
- **The first user text.** The agent keeps a `first_user_text: Option<String>`. `run_turn` sets it from the raw
  `user_input`, via `preview_text`, when it is `None` and the input is not a slash command handled before the
  turn snapshot. The simplest correct place is right where `turn_preview` is computed.
  - `clear_conversation` and switching reset it.
  - `restore_session` sets it from `meta.first_user_text`.
- **`persist()`.**
  - With `File`: as today.
  - With `Store`:
    - if `id` is `None`, allocate `SessionStore::new_id(now_unix_ms())` and set `created_unix = now_unix()`;
    - build the state as today, plus `meta = SessionMeta { id, created_unix, updated_unix: now_unix(),
      first_user_text: self.first_user_text.clone().unwrap_or_default(), turns: count_turns(&self.history) }`;
    - `store.save`, then `store.prune(SESSIONS_KEPT)`. A failure is logged, as today.
  - `persist` needs `&mut self` to set the id. Change its signature, and those of `persist_if_owned` and their
    callers, accordingly.
- **The B1 gate is unchanged.** `run_turn` still persists only when the turn succeeded or the slot is already
  owned. So a fresh agent whose first turn fails creates no file.
- **`clear_conversation()` with `Store`.**
  1. `persist_if_owned()`, saving the outgoing conversation.
  2. Do everything it does today except the final `persist()`.
  3. Set `id = None`, `first_user_text = None` and `session_owns_slot = false`. The next successful turn creates
     a new file.

  With `File`, today's behaviour is kept exactly.
- **`restore_session(state)`.** Calls `restore(state.clone())`. With `Store`, it sets `id = Some(state.meta.id)`
  (when non-empty) and `created_unix = state.meta.created_unix`, and sets `first_user_text`.

- [ ] **Step 1: Write the failing tests**

```rust
// ---- sessions per project ----

fn store_agent(store_dir: &Path, responses: Vec<Vec<StreamEvent>>) -> (Agent, UnboundedReceiver<AgentEvent>) {
    let (mut agent, rx) = build_agent_with_backend(
        Arc::new(MockBackend::new(responses)),
        store_dir.join("unused.json"),
    );
    agent.set_session_store(crate::session::SessionStore::new(store_dir.to_path_buf()));
    (agent, rx)
}

#[tokio::test]
async fn a_failed_first_turn_creates_no_conversation_file() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("proj");
    let (mut agent, _rx) = build_agent_with_backend(Arc::new(FailingBackend), store_dir.join("unused.json"));
    agent.set_session_store(crate::session::SessionStore::new(store_dir.clone()));
    let _ = agent.run_turn("hello".into(), Path::new("."), CancellationToken::new()).await;
    assert!(crate::session::SessionStore::new(store_dir).list().is_empty());
}

#[tokio::test]
async fn two_fresh_agents_make_two_conversations() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("proj");
    for text in ["first chat", "second chat"] {
        let (mut agent, _rx) = store_agent(&store_dir, vec![text_response("ok")]);
        agent.run_turn(text.into(), Path::new("."), CancellationToken::new()).await.unwrap();
    }
    let list = crate::session::SessionStore::new(store_dir).list();
    assert_eq!(list.len(), 2);
    let firsts: Vec<&str> = list.iter().map(|m| m.first_user_text.as_str()).collect();
    assert_eq!(firsts, vec!["second chat", "first chat"]);
    assert!(list.iter().all(|m| m.turns == 1));
}

#[tokio::test]
async fn later_turns_update_the_same_file() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("proj");
    let (mut agent, _rx) = store_agent(&store_dir, vec![text_response("a"), text_response("b")]);
    agent.run_turn("one".into(), Path::new("."), CancellationToken::new()).await.unwrap();
    agent.run_turn("two".into(), Path::new("."), CancellationToken::new()).await.unwrap();
    let list = crate::session::SessionStore::new(store_dir).list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].turns, 2);
    assert_eq!(list[0].first_user_text, "one");
}

#[tokio::test]
async fn clear_keeps_the_old_conversation_and_the_next_turn_starts_a_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("proj");
    let (mut agent, _rx) = store_agent(&store_dir, vec![text_response("a"), text_response("b")]);
    agent.run_turn("before clear".into(), Path::new("."), CancellationToken::new()).await.unwrap();
    agent.clear_conversation();
    let store = crate::session::SessionStore::new(store_dir);
    assert_eq!(store.list().len(), 1, "/clear alone writes nothing new");
    assert_eq!(store.list()[0].first_user_text, "before clear");
    agent.run_turn("after clear".into(), Path::new("."), CancellationToken::new()).await.unwrap();
    let firsts: Vec<String> = store.list().into_iter().map(|m| m.first_user_text).collect();
    assert_eq!(firsts, vec!["after clear".to_string(), "before clear".to_string()]);
}

#[tokio::test]
async fn restore_session_continues_the_same_file() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("proj");
    let (mut agent, _rx) = store_agent(&store_dir, vec![text_response("a")]);
    agent.run_turn("original".into(), Path::new("."), CancellationToken::new()).await.unwrap();
    let store = crate::session::SessionStore::new(store_dir.clone());
    let id = store.list()[0].id.clone();

    let (mut agent2, _rx2) = store_agent(&store_dir, vec![text_response("b")]);
    agent2.restore_session(store.load(&id).unwrap());
    agent2.run_turn("more".into(), Path::new("."), CancellationToken::new()).await.unwrap();
    let list = store.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].turns, 2);
    assert_eq!(list[0].first_user_text, "original");
}
```

`FailingBackend`, `MockBackend`, `text_response` and `build_agent_with_backend` already exist in `tests.rs`. Check
the exact names with `grep -n "struct FailingBackend\|fn build_agent_with_backend\|fn text_response"
crates/aivyx-core/src/agent/tests.rs`.

- [ ] **Step 2: RED.** `cargo test -p aivyx-core sessions` (or the test names) fails to compile.
- [ ] **Step 3: Implement the behaviour above.** All existing tests must keep passing unchanged, which proves
  the `File` mode is intact.
- [ ] **Step 4: GREEN, then the full verification.**
- [ ] **Step 5: Commit** (`agent: persist each conversation to its own file; /clear starts a new one`).

---

### Task 3: `/sessions` and `/resume N`

**Files:**
- Create: `crates/aivyx-core/src/session_list.rs` (plus `pub mod session_list;` in `lib.rs`) and
  `crates/aivyx-core/src/agent/session_commands.rs` (plus `mod session_commands;` in `agent/mod.rs`)
- Modify:
  - `agent/mod.rs`: interception, `session_switching` flag, `switch_to`;
  - `agent/types.rs`: `SessionSwitched`;
  - `commands.rs`;
  - the compile-only arms in `crates/aivyx-tui/src/app.rs` and `crates/aivyx-acp/src/translate.rs`.
- Test: unit tests in `session_list.rs`; agent tests appended to `agent/tests.rs`

**Interfaces:**
- Produces:
  - `pub fn when_label(ts: i64, now: i64, offset_secs: i32) -> String`
  - `pub fn sessions_listing(metas: &[SessionMeta], current_id: Option<&str>, now: i64, offset_secs: i32) -> String`
  - `AgentEvent::SessionSwitched { history: Vec<Message>, tasks: Vec<Task> }`
  - `Agent::enable_session_switching(&mut self)`. It sets `session_switching = true`; the TUI build calls it.
- Commands table:
  - `/sessions`: description `List this project's saved conversations`.
  - `/resume`: description `Switch to saved conversation N (/resume N)`.
  - `/clear`: description becomes `Start a new conversation (the old one stays in /sessions)`.
  - Both new entries take `CommandTier::AgentState`.

**Behaviour:**
- **`when_label`.** Local time is `ts + offset`. Compare day numbers with `div_euclid(86_400)`:
  - same day as `now + offset` → `today HH:MM`;
  - one day before → `yesterday HH:MM`;
  - otherwise `{Wkd} {d} {Mon} HH:MM`, from `chrono::DateTime::from_timestamp(local, 0)`'s `weekday()`, `day()`
    and `month0()`, mapped through name tables `["Mon",…]` and `["Jan",…]`.
- **`sessions_listing`.** Formats each row as in Global Constraints. With an empty list it returns the
  none-saved string.
- **`/sessions`.**
  - No `Store` target: `info` with the no-store string.
  - Otherwise: `info(sessions_listing(&store.list(), current_id, now_unix(), local_offset))`. `local_offset`
    is `chrono::Local::now().offset().local_minus_utc()`.
- **`/resume N`.**
  - Not `session_switching`, or no store: `info("/resume isn't available here.")`.
  - A bad argument: the usage string.
  - `N` out of range (`0` or `> list.len()`): the out-of-range string with `K = list.len()`.
  - Otherwise `switch_to(&list[N-1].id)`, then `info("Resumed conversation N (T turns)")`, with T from that meta.
- **`switch_to(id)`.**
  1. `persist_if_owned()`, saving the current conversation.
  2. `store.load(id)`. If `None`, `notify("Couldn't load that conversation.")` and stop.
  3. Reset what `clear_conversation` resets, without its persist or its `ConversationCleared` event: tasks,
     mission plan, `pool.close_all()`, router `forget_session`, `last_routed`, the undo ledger and pending notes.
  4. `restore_session(state.clone())`, then `pool.seed_dehydrated(state.specialist_sessions.clone())` if a pool
     is set.
  5. Emit `SessionSwitched { history: self.history.clone(), tasks: self.tasks snapshot }`.
- **Interception.** In `run_turn`, directly after the `/test` interception and in the same shape: run, emit
  `TurnComplete`, return `Ok(())`. Never a model turn.
- **Compile arms.** Until Task 4:
  - TUI: `AgentEvent::SessionSwitched { .. } => {}` in `handle_agent_event`, and `String::new()` in the text
    extraction;
  - ACP: `AgentEvent::SessionSwitched { .. } => return None`.

- [ ] **Step 1: Failing tests.** In `session_list.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // 2026-10-03 14:02:00 UTC is a Saturday.
    const NOW: i64 = 1_791_036_120;

    #[test]
    fn when_label_today_yesterday_and_older() {
        assert_eq!(when_label(NOW, NOW, 0), "today 14:02");
        assert_eq!(when_label(NOW - 86_400, NOW, 0), "yesterday 14:02");
        assert_eq!(when_label(NOW - 4 * 86_400, NOW, 0), "Tue 29 Sep 14:02");
        // +8 h: 22:02 local, still today.
        assert_eq!(when_label(NOW, NOW, 8 * 3600), "today 22:02");
    }

    #[test]
    fn listing_marks_the_current_one_and_pluralises() {
        let metas = vec![
            SessionMeta { id: "b".into(), created_unix: NOW, updated_unix: NOW, first_user_text: "fix it".into(), turns: 6 },
            SessionMeta { id: "a".into(), created_unix: NOW - 86_400, updated_unix: NOW - 86_400, first_user_text: "hello".into(), turns: 1 },
        ];
        assert_eq!(
            sessions_listing(&metas, Some("b"), NOW, 0),
            "Conversations for this project (newest first — /resume N to switch):\n\
             1  today 14:02 · 6 turns · \"fix it\"  (current)\n\
             2  yesterday 14:02 · 1 turn · \"hello\""
        );
        assert_eq!(sessions_listing(&[], None, NOW, 0), "No saved conversations for this project yet.");
    }
}
```

`NOW` was checked with `date -u -d @1791036120` (Sat 3 Oct 2026 14:02:00 UTC); four days earlier is Tue 29 Sep.

Agent tests (append to `tests.rs`):

```rust
#[tokio::test]
async fn sessions_lists_saved_conversations() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("proj");
    let (mut agent, mut rx) = store_agent(&store_dir, vec![text_response("a")]);
    agent.run_turn("first".into(), Path::new("."), CancellationToken::new()).await.unwrap();
    drain(&mut rx);
    agent.run_turn("/sessions".into(), Path::new("."), CancellationToken::new()).await.unwrap();
    let text = infos(&mut rx).join("\n");
    assert!(text.contains("1  today ") && text.contains("· 1 turn · \"first\"  (current)"), "{text}");
}

#[tokio::test]
async fn resume_n_switches_conversations_and_restores_history_tasks_and_undo() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("proj");
    let (mut agent, mut rx) = store_agent(&store_dir, vec![text_response("a"), text_response("b")]);
    agent.enable_session_switching();
    agent.run_turn("older chat".into(), Path::new("."), CancellationToken::new()).await.unwrap();
    agent.tasks.lock().unwrap().push(crate::session::Task {
        id: 1, text: "task from older".into(), status: crate::session::TaskStatus::Pending,
    });
    agent.clear_conversation();
    agent.run_turn("newer chat".into(), Path::new("."), CancellationToken::new()).await.unwrap();
    drain(&mut rx);

    agent.run_turn("/resume 2".into(), Path::new("."), CancellationToken::new()).await.unwrap();
    let events = drain(&mut rx);
    let switched = events.iter().find_map(|e| match e {
        AgentEvent::SessionSwitched { history, tasks } => Some((history.clone(), tasks.clone())),
        _ => None,
    }).expect("SessionSwitched");
    assert_eq!(switched.0[0].text_content(), "older chat");
    assert_eq!(switched.1.len(), 1);
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Info(t) if t == "Resumed conversation 2 (1 turn)")));
    assert_eq!(agent.history[0].text_content(), "older chat");
    // The newer conversation was kept, and switching back works.
    let store = crate::session::SessionStore::new(store_dir);
    assert_eq!(store.list().len(), 2);
}

#[tokio::test]
async fn resume_out_of_range_and_bad_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("proj");
    let (mut agent, mut rx) = store_agent(&store_dir, vec![text_response("a")]);
    agent.enable_session_switching();
    agent.run_turn("only".into(), Path::new("."), CancellationToken::new()).await.unwrap();
    drain(&mut rx);
    for (input, expected) in [
        ("/resume 0", "There are only 1 saved conversations for this project (see /sessions)."),
        ("/resume 5", "There are only 1 saved conversations for this project (see /sessions)."),
        ("/resume", "Use /resume N — /sessions lists them."),
        ("/resume two", "Use /resume N — /sessions lists them."),
    ] {
        agent.run_turn(input.into(), Path::new("."), CancellationToken::new()).await.unwrap();
        assert_eq!(infos(&mut rx), vec![expected.to_string()], "{input}");
    }
}

#[tokio::test]
async fn resume_is_refused_where_switching_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let (mut agent, mut rx) = store_agent(&dir.path().join("proj"), vec![]);
    agent.run_turn("/resume 1".into(), Path::new("."), CancellationToken::new()).await.unwrap();
    assert_eq!(infos(&mut rx), vec!["/resume isn't available here.".to_string()]);
}
```

Also test the undo ledger: in `resume_n_switches…`, or a separate test, give the older conversation a non-empty
`undo` before `/clear` and assert it's back after `/resume 2`. Use whatever simple way the ledger can be filled,
for example pushing a `TurnMark` directly if its fields are public in-crate. Grammar: "There are only 1 saved
conversations" is what the spec's template gives. Keep it verbatim.

- [ ] **Step 2: RED.** **Step 3: Implement.** **Step 4: GREEN plus full verification.**
- [ ] **Step 5: Commit** (`/sessions and /resume N: list and switch saved conversations`).

---

### Task 4: CLI, wiring, TUI, docs

**Files:**
- Modify:
  - `crates/aivyx/src/main.rs`: the `resume` field and its three `cli.resume` uses;
  - `crates/aivyx/src/agent_builder.rs`: the `--auto` conflict at ~420, the restore block at ~1459;
  - `crates/aivyx-tui/src/app.rs`;
  - `crates/aivyx-acp/src/translate.rs` (keep `None`);
  - `README.md`, `CLAUDE.md`
- Test: `main.rs`/`agent_builder.rs` test modules; `app.rs` tests

**Behaviour:**
- **The CLI flag.**

  ```rust
  /// Resume a saved conversation for this project: the latest with bare
  /// --resume, or number N from /sessions with --resume=N.
  #[arg(long, value_name = "N", num_args = 0..=1, require_equals = true)]
  resume: Option<Option<usize>>,
  ```

  The `--acp`/`--mcp-server`/`--auto` conflict checks use `cli.resume.is_some()`. Messages are unchanged.
- **The builder.** In `agent_builder.rs`, replace the restore block with a pure helper
  `pick_resume(list: &[SessionMeta], resume: Option<Option<usize>>) -> anyhow::Result<Option<String>>`:
  - `None` (no flag) → `Ok(None)`;
  - `Some(None)` (bare) → the first id, or `Ok(None)` with today's "no resumable session" info log when empty;
  - `Some(Some(n))` → `Ok(Some(list[n-1].id))` when `1 <= n <= len`, otherwise
    `bail!("There are only {len} saved conversations for this project (see /sessions).")`.

  Then:
  - `let store = SessionStore::for_project(&cwd)`. When it is `Some`:
    - run `store.migrate_legacy(&session_file_path(&cwd)?)`, logging any error;
    - pick the conversation; when one is picked, `agent.restore_session(state.clone())`, seed the specialist pool
      as today, and set `restored = Some(state)`;
    - `agent.set_session_store(store)`.
  - When it is `None`, log the existing warning.
- **The TUI** (`main.rs` TUI path, before `aivyx_tui::run`). Call `built.agent.enable_session_switching()`. Only
  the TUI does this, not ACP.
- **The TUI** (`app.rs`).
  - **`SessionSwitched { history, tasks }`.** Reset as `ConversationCleared` does, then set
    `self.transcript = seed_transcript(&history)` and `self.tasks = tasks`. The agent's `Info` follows with the
    "Resumed conversation…" line.
  - **Busy refusal.** For `/resume`, use the same `else if` pattern as `/test`, so either command with a turn
    running shows the busy string. Put both commands in one condition.
  - **The `/clear` branch** (~line 430). Replace `agent.notify("Conversation cleared.")` with
    `agent.info("New conversation — the previous one is in /sessions.")`. `info` is `pub(crate)` in aivyx-core,
    so make it `pub`; if you'd rather not widen it, keep `notify` but use the new text. Update any TUI test that
    asserts the old text.
  - **The startup notice for a restored session.** `App::new` currently says `resumed previous session (N
    messages restored)`. Keep it.
- **Docs.**
  - README:
    - replace "one session per project" wording with the store layout, the 20-kept rule, `/sessions`,
      `/resume N`, `--resume[=N]`, `/clear` keeping the old conversation, and the migration;
    - in the ACP section, say ACP conversations are saved there too but can't be switched in the editor.
  - `CLAUDE.md`, the "Config lives at…" bullet: "Sessions persist under
    `~/.local/state/aivyx-coder/sessions/<project-key>/`, one file per conversation (newest 20 kept)…".
  - `CLAUDE.md`, the aivyx-core row: add `session_list` + `agent/session_commands.rs`.

- [ ] **Step 1: Failing tests**

```rust
    // agent_builder.rs tests
    fn metas(n: usize) -> Vec<aivyx_core::session::SessionMeta> {
        (0..n).map(|i| aivyx_core::session::SessionMeta { id: format!("id{i}"), ..Default::default() }).collect()
    }

    #[test]
    fn pick_resume_cases() {
        assert_eq!(pick_resume(&metas(3), None).unwrap(), None);
        assert_eq!(pick_resume(&metas(3), Some(None)).unwrap(), Some("id0".into()));
        assert_eq!(pick_resume(&metas(0), Some(None)).unwrap(), None);
        assert_eq!(pick_resume(&metas(3), Some(Some(2))).unwrap(), Some("id1".into()));
        for bad in [0, 4] {
            assert_eq!(
                pick_resume(&metas(3), Some(Some(bad))).unwrap_err().to_string(),
                "There are only 3 saved conversations for this project (see /sessions)."
            );
        }
    }
```

```rust
    // main.rs tests (use clap's Cli::try_parse_from)
    #[test]
    fn resume_flag_forms() {
        assert_eq!(Cli::try_parse_from(["aivyx-coder"]).unwrap().resume, None);
        assert_eq!(Cli::try_parse_from(["aivyx-coder", "--resume"]).unwrap().resume, Some(None));
        assert_eq!(Cli::try_parse_from(["aivyx-coder", "--resume=2"]).unwrap().resume, Some(Some(2)));
        assert_eq!(Cli::try_parse_from(["aivyx-coder", "--resume=0"]).unwrap().resume, Some(Some(0)));
        assert!(Cli::try_parse_from(["aivyx-coder", "--resume=x"]).is_err());
    }
```

`--resume=0` parses; `pick_resume` rejects it. If `Cli` lives under another name or in another module, adapt the
path. Look for an existing `try_parse_from` test.

```rust
    // app.rs tests
    #[test]
    fn session_switched_rebuilds_the_transcript() {
        let mut app = App::new(None, PlanMode::new());
        app.transcript.push(ChatLine::Info("old".into()));
        app.handle_agent_event(AgentEvent::SessionSwitched {
            history: vec![Message::text(Role::User, "older chat")],
            tasks: vec![],
        });
        assert!(matches!(&app.transcript[..], [ChatLine::User(t)] if t == "older chat"));
    }
```

- [ ] **Step 2: RED.** **Step 3: Implement.** **Step 4: GREEN plus full verification.**
- [ ] **Step 5: Commit** (`--resume[=N], session store wiring + migration, TUI switching, docs`).
