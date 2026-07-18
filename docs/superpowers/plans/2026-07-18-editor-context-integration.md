# Editor/IDE Context Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the agent live awareness of what the user is looking at in their editor (open file, cursor, selection) via a polled, per-project JSON context file, auto-injected into the system prompt every turn as metadata only — no editor plugin ships in this phase, just the aivyx-coder-side contract and consumption.

**Architecture:** A new standalone `editor_context` module in `aivyx-core` owns the file-location keying, the JSON schema, and raw parsing. `Agent` (in `agent/mod.rs`) owns all semantic validation (schema version, staleness, workspace match, `deny_paths`) and the per-turn refresh/injection, mirroring the existing `AGENTS.md` (`refresh_agents_files`/`agents_files_text`) pattern exactly. A new `[editor_context]` config section and a one-line `aivyx-sandbox` visibility widening complete the wiring from `main.rs`.

**Tech Stack:** Rust, `serde`/`serde_json` (already a dependency), `time` crate (new dependency, for RFC 3339 timestamp parsing).

## Global Constraints

- Full authoritative spec: `docs/superpowers/specs/2026-07-18-editor-context-integration-design.md` — read it before starting; every decision below traces back to it.
- JSON schema is fixed exactly as specified: `schema_version` (int), `workspace_root` (string, absolute path), `file` (string, relative to `workspace_root`), `cursor: { line, column }` (1-indexed, required), `selection: { start_line, end_line }` (1-indexed, inclusive, optional — omitted not `null`), `updated_at` (RFC 3339 string).
- Staleness threshold: exactly 5 minutes, hardcoded, not configurable.
- Injected text is metadata only — **never** file content. This is a security property, not a style preference; a test in Task 2 exists specifically to guard it.
- No editor plugin code (VS Code, Neovim, or otherwise) ships in this plan.
- `[editor_context] enabled` defaults to `true` (matches `repo_map`/`agents_file`'s own default-enabled convention — the feature is a no-op until a context file exists).

---

### Task 1: `editor_context` module — schema, path keying, raw read/parse

**Files:**
- Create: `crates/aivyx-core/src/editor_context.rs`
- Modify: `crates/aivyx-core/src/lib.rs`
- Modify: `crates/aivyx-core/Cargo.toml`

**Interfaces:**
- Produces: `pub(crate) const SCHEMA_VERSION: u32`; `pub(crate) fn editor_context_file_path(cwd: &Path) -> Option<PathBuf>`; `pub(crate) struct EditorContext { schema_version: u32, workspace_root: PathBuf, file: PathBuf, cursor: Cursor, selection: Option<Selection>, updated_at: OffsetDateTime }` (all fields `pub(crate)`); `pub(crate) struct Cursor { line: u32, column: u32 }` (fields `pub(crate)`); `pub(crate) struct Selection { start_line: u32, end_line: u32 }` (fields `pub(crate)`); `pub(crate) async fn read_editor_context(path: &Path) -> Option<EditorContext>`. Task 2's `Agent::refresh_editor_context` consumes all of these by their exact names.

- [ ] **Step 1: Add the `time` dependency**

Use the Edit tool on `crates/aivyx-core/Cargo.toml`:

old_string:
```
tokio = { version = "1.52.3", features = ["rt-multi-thread", "macros"] }
tokio-util = "0.7.18"
tracing = "0.1.44"
```

new_string:
```
time = { version = "0.3.53", features = ["parsing", "formatting", "serde"] }
tokio = { version = "1.52.3", features = ["rt-multi-thread", "macros"] }
tokio-util = "0.7.18"
tracing = "0.1.44"
```

- [ ] **Step 2: Verify the dependency resolves and its serde RFC 3339 helper compiles**

```bash
cargo check -p aivyx-core 2>&1 | tail -20
```

Expected: succeeds (this only adds an unused dependency at this point — no code references it yet — so the check is purely "does the version/feature combination resolve and build," not a functional test). If the `parsing`/`formatting`/`serde` feature names don't exist for this exact `time` version, the error will name the invalid feature — adjust to whatever the crate's actual Cargo.toml exposes for this version (check `~/.cargo/registry/src/*/time-0.3.53/Cargo.toml`'s `[features]` section if the plain feature names above don't resolve) and re-run this step until it succeeds before continuing.

- [ ] **Step 3: Create crates/aivyx-core/src/editor_context.rs**

Use the Write tool to create `crates/aivyx-core/src/editor_context.rs` with exactly this content:

```rust
//! Live editor context (open file, cursor, selection): a small, versioned
//! JSON contract any editor integration can write to. aivyx-coder only
//! reads it — see `docs/superpowers/specs/2026-07-18-editor-context-integration-design.md`
//! for the full design and the rationale for keeping this metadata-only
//! (no file content ever flows through this channel).

use std::path::{Path, PathBuf};

use serde::Deserialize;
use time::OffsetDateTime;

/// Bumped if the on-disk shape changes incompatibly. A file reporting any
/// other value is treated as absent (not a best-effort parse) — this is a
/// machine-to-machine contract, not a human-edited config file.
pub(crate) const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
pub(crate) struct EditorContext {
    pub(crate) schema_version: u32,
    pub(crate) workspace_root: PathBuf,
    pub(crate) file: PathBuf,
    pub(crate) cursor: Cursor,
    pub(crate) selection: Option<Selection>,
    #[serde(with = "time::serde::rfc3339")]
    pub(crate) updated_at: OffsetDateTime,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Cursor {
    pub(crate) line: u32,
    pub(crate) column: u32,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Selection {
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
}

/// Where an editor integration writes its context for this project: one
/// file per project directory, keyed by a stable hash of the canonicalized
/// `cwd`, under the platform state dir
/// (`~/.local/state/aivyx-coder/editor-context/` on Linux) — identical
/// construction to `session::session_file_path`, minus the human-readable
/// name prefix (this file is machine-written and machine-read only, never
/// resumed-by-eye the way a session file might be inspected).
///
/// `None` when no home/state directory can be determined at all.
pub(crate) fn editor_context_file_path(cwd: &Path) -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "aivyx-coder")?;
    let state_dir = dirs
        .state_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dirs.data_local_dir().to_path_buf());

    let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let key = format!("{:016x}", fnv1a(canonical.to_string_lossy().as_bytes()));
    Some(
        state_dir
            .join("editor-context")
            .join(format!("{key}.json")),
    )
}

/// FNV-1a, inlined for the same reason `session::fnv1a` is: the key must
/// be stable across program versions, and `std`'s `DefaultHasher` doesn't
/// guarantee that.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Reads and parses the context file at `path`. `None` on any I/O error
/// or malformed JSON — an editor integration that isn't running, or a
/// leftover file from a crashed one, is a normal, silent state, not a
/// user-facing error. Does NOT validate `schema_version`, staleness, or
/// `workspace_root` — that's `Agent::refresh_editor_context`'s job (it
/// needs `cwd` and `deny_paths`, which this module deliberately doesn't
/// know about, to do those checks).
pub(crate) async fn read_editor_context(path: &Path) -> Option<EditorContext> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    serde_json::from_str(&content).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_directory_keys_to_the_same_path() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        let a1 = editor_context_file_path(dir_a.path()).expect("state dir should exist in tests");
        let a2 = editor_context_file_path(dir_a.path()).unwrap();
        let b = editor_context_file_path(dir_b.path()).unwrap();

        assert_eq!(a1, a2, "same directory must key to the same context file");
        assert_ne!(a1, b, "different directories must not collide");
        assert!(a1.extension().is_some_and(|e| e == "json"));
        assert!(a1.to_string_lossy().contains("editor-context"));
    }

    #[test]
    fn path_survives_a_non_canonical_spelling_of_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        let dotted = dir.path().join(".");

        assert_eq!(
            editor_context_file_path(dir.path()).unwrap(),
            editor_context_file_path(&dotted).unwrap()
        );
    }

    #[tokio::test]
    async fn reads_and_parses_a_valid_context_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context.json");
        tokio::fs::write(
            &path,
            r#"{
                "schema_version": 1,
                "workspace_root": "/home/user/project",
                "file": "src/foo.rs",
                "cursor": { "line": 42, "column": 8 },
                "selection": { "start_line": 40, "end_line": 45 },
                "updated_at": "2026-07-18T12:00:00Z"
            }"#,
        )
        .await
        .unwrap();

        let context = read_editor_context(&path).await.expect("should parse");
        assert_eq!(context.schema_version, 1);
        assert_eq!(context.workspace_root, PathBuf::from("/home/user/project"));
        assert_eq!(context.file, PathBuf::from("src/foo.rs"));
        assert_eq!(context.cursor.line, 42);
        assert_eq!(context.cursor.column, 8);
        let selection = context.selection.expect("selection should be present");
        assert_eq!(selection.start_line, 40);
        assert_eq!(selection.end_line, 45);
    }

    #[tokio::test]
    async fn reads_a_valid_context_file_with_no_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context.json");
        tokio::fs::write(
            &path,
            r#"{
                "schema_version": 1,
                "workspace_root": "/home/user/project",
                "file": "src/foo.rs",
                "cursor": { "line": 1, "column": 1 },
                "updated_at": "2026-07-18T12:00:00Z"
            }"#,
        )
        .await
        .unwrap();

        let context = read_editor_context(&path).await.expect("should parse");
        assert!(context.selection.is_none());
    }

    #[tokio::test]
    async fn missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        assert!(read_editor_context(&path).await.is_none());
    }

    #[tokio::test]
    async fn malformed_json_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context.json");
        tokio::fs::write(&path, "{ not valid json").await.unwrap();
        assert!(read_editor_context(&path).await.is_none());
    }
}
```

- [ ] **Step 4: Declare the module in lib.rs**

Use the Edit tool on `crates/aivyx-core/src/lib.rs`:

old_string:
```
pub mod agent;
pub mod architect;
pub mod council;
pub mod delegate;
pub mod edit_blocks;
pub mod session;
pub mod wiki;
```

new_string:
```
pub mod agent;
pub mod architect;
pub mod council;
pub mod delegate;
pub mod edit_blocks;
pub mod editor_context;
pub mod session;
pub mod wiki;
```

(Alphabetically ordered, matching the existing list's own ordering convention; `pub mod` matches every other declaration in this file — none are declared as bare private `mod`, even though `editor_context`'s own items are all `pub(crate)` and don't need external re-export. Consistency with the existing 100%-`pub mod` convention, not a functional requirement.)

- [ ] **Step 5: Add `tempfile` as a dev-dependency if not already present, then run the tests**

Check first — `tempfile` may already be a dev-dependency:

```bash
grep -A3 "\[dev-dependencies\]" crates/aivyx-core/Cargo.toml
```

If `tempfile = "3.27.0"` is already listed (it should be, per the existing `[dev-dependencies]` block), no change needed. Then:

```bash
cargo test -p aivyx-core editor_context 2>&1 | tail -30
```

Expected: 5 new tests (`same_directory_keys_to_the_same_path`,
`path_survives_a_non_canonical_spelling_of_the_same_directory`,
`reads_and_parses_a_valid_context_file`,
`reads_a_valid_context_file_with_no_selection`, `missing_file_returns_none`,
`malformed_json_returns_none` — 6 total) all pass.

- [ ] **Step 6: Full workspace sanity check**

```bash
cargo build --workspace 2>&1 | tail -10
cargo clippy --workspace --all-targets 2>&1 | tail -20
```

Expected: build succeeds, clippy stays clean (zero warnings) — this new module isn't referenced from anywhere else yet, so this just confirms it compiles cleanly in isolation.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-core/Cargo.toml crates/aivyx-core/src/lib.rs crates/aivyx-core/src/editor_context.rs
git commit -m "Add editor_context module: schema, path keying, raw read/parse"
```

---

### Task 2: `Agent` integration — config, refresh, prompt injection

**Files:**
- Modify: `crates/aivyx-core/src/agent/types.rs`
- Modify: `crates/aivyx-core/src/agent/mod.rs`

**Interfaces:**
- Consumes: `crate::editor_context::{SCHEMA_VERSION, EditorContext, editor_context_file_path, read_editor_context}` from Task 1 (exact names, already committed). `aivyx_sandbox::path_is_denied` — **not yet public** (still `pub(crate)` in `aivyx-sandbox`, widened in Task 3) — this task's code will not compile standalone until Task 3 lands; that's expected and fine, since Task 3 immediately follows in the same branch before any whole-branch build is required to pass. If your task-runner insists on a fully green build before moving on, note this dependency explicitly in your report rather than trying to work around it (e.g. do not stub or duplicate `path_is_denied` locally — Task 3's job is the real fix).
- Produces: `Agent::set_editor_context(&mut self, deny_paths: Vec<PathBuf>)` and the `editor_context_text` field/injection behavior — Task 3's `main.rs` wiring calls this exact method with this exact signature.

- [ ] **Step 1: Add `EditorContextConfig` to agent/types.rs**

Read `crates/aivyx-core/src/agent/types.rs` first to confirm current content, then use the Edit tool:

old_string:
```
/// Where to find the user-global `AGENTS.md` (if resolvable) and the
/// per-file token budget both the global and project files share.
pub(crate) struct AgentsFileConfig {
    pub(crate) global_path: Option<PathBuf>,
    pub(crate) budget_tokens: u32,
}
```

new_string:
```
/// Where to find the user-global `AGENTS.md` (if resolvable) and the
/// per-file token budget both the global and project files share.
pub(crate) struct AgentsFileConfig {
    pub(crate) global_path: Option<PathBuf>,
    pub(crate) budget_tokens: u32,
}

/// `deny_paths` needed to check a reported editor-context file path before
/// surfacing it — see `Agent::refresh_editor_context`. No budget/enable
/// fields here: unlike `AGENTS.md`, there's no token-budget concept for a
/// one-line status string, and `Some`/`None` on the outer
/// `editor_context_config` field is itself the enable/disable signal,
/// mirroring `AgentsFileConfig`'s own pattern.
pub(crate) struct EditorContextConfig {
    pub(crate) deny_paths: Vec<PathBuf>,
}
```

- [ ] **Step 2: Run a quick check that the new struct compiles in isolation**

```bash
cargo check -p aivyx-core 2>&1 | tail -20
```

Expected: succeeds — `EditorContextConfig` isn't referenced from `agent/mod.rs` yet, so this only confirms the struct itself is well-formed (no missing imports for `PathBuf` — it should already be imported in `types.rs` since `AgentsFileConfig` uses it).

- [ ] **Step 3: Add the two new imports to agent/mod.rs**

Use the Edit tool on `crates/aivyx-core/src/agent/mod.rs`:

old_string:
```
use futures::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::edit_blocks::{self, BlockParse};
use crate::session::{self, SessionState, Task};
```

new_string:
```
use futures::StreamExt;
use time::OffsetDateTime;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::edit_blocks::{self, BlockParse};
use crate::editor_context;
use crate::session::{self, SessionState, Task};
```

- [ ] **Step 4: Import `EditorContextConfig` into scope**

old_string:
```
use types::{AgentsFileConfig, VerificationConfig};
```

new_string:
```
use types::{AgentsFileConfig, EditorContextConfig, VerificationConfig};
```

- [ ] **Step 5: Add the two new `Agent` struct fields**

old_string:
```
    /// `AGENTS.md` support when configured (`set_agents_file`); `None`
    /// disables the feature entirely (both files).
    agents_file_config: Option<AgentsFileConfig>,
    /// The rendered slice appended to the system prompt; also counted by
    /// the context estimator — a ~1k-token block compaction can't see would
    /// silently eat the window's headroom.
    repo_map_text: Option<String>,
    /// Combined, labeled rendering of the project's and/or user's
    /// `AGENTS.md` — re-rendered once per turn by `refresh_agents_files`,
    /// mirroring `repo_map_text`'s own per-turn cadence.
    agents_files_text: Option<String>,
```

new_string:
```
    /// `AGENTS.md` support when configured (`set_agents_file`); `None`
    /// disables the feature entirely (both files).
    agents_file_config: Option<AgentsFileConfig>,
    /// Editor-context support when configured (`set_editor_context`);
    /// `None` disables the feature entirely.
    editor_context_config: Option<EditorContextConfig>,
    /// The rendered slice appended to the system prompt; also counted by
    /// the context estimator — a ~1k-token block compaction can't see would
    /// silently eat the window's headroom.
    repo_map_text: Option<String>,
    /// Combined, labeled rendering of the project's and/or user's
    /// `AGENTS.md` — re-rendered once per turn by `refresh_agents_files`,
    /// mirroring `repo_map_text`'s own per-turn cadence.
    agents_files_text: Option<String>,
    /// One-line "currently open in editor" status, re-rendered once per
    /// turn by `refresh_editor_context` — same per-turn cadence as
    /// `agents_files_text`/`repo_map_text`. Metadata only, deliberately —
    /// see the module doc on `editor_context` for why file content never
    /// flows through this field.
    editor_context_text: Option<String>,
```

- [ ] **Step 6: Initialize the two new fields in `Agent::new`**

old_string:
```
            repo_map: None,
            agents_file_config: None,
            repo_map_text: None,
            agents_files_text: None,
```

new_string:
```
            repo_map: None,
            agents_file_config: None,
            editor_context_config: None,
            repo_map_text: None,
            agents_files_text: None,
            editor_context_text: None,
```

- [ ] **Step 7: Add `Agent::set_editor_context`**

old_string:
```
    pub fn set_agents_file(&mut self, global_path: Option<PathBuf>, budget_tokens: u32) {
        self.agents_file_config = Some(AgentsFileConfig {
            global_path,
            budget_tokens,
        });
    }
```

new_string:
```
    pub fn set_agents_file(&mut self, global_path: Option<PathBuf>, budget_tokens: u32) {
        self.agents_file_config = Some(AgentsFileConfig {
            global_path,
            budget_tokens,
        });
    }

    /// Enables editor-context awareness: a per-project JSON file an editor
    /// integration writes to (see the `editor_context` module), re-read
    /// and surfaced as a one-line system-prompt addition every turn.
    /// `deny_paths` is checked against the reported file path before it's
    /// ever surfaced, same as every other path-reporting tool in this
    /// project.
    pub fn set_editor_context(&mut self, deny_paths: Vec<PathBuf>) {
        self.editor_context_config = Some(EditorContextConfig { deny_paths });
    }
```

- [ ] **Step 8: Add `Agent::refresh_editor_context`**

old_string:
```
    /// Re-renders the map off the async runtime. Best-effort: a failure
    /// just means this turn goes without a map.
    async fn refresh_repo_map(&mut self) {
```

new_string:
```
    /// Re-reads the editor-context file (if configured) and stores a
    /// one-line "currently open in editor" status, or clears it to `None`
    /// on any of: feature disabled, file missing/unreadable/malformed,
    /// unrecognized `schema_version`, stale `updated_at` (>5 minutes old),
    /// `workspace_root` not matching this session's own `cwd`, or the
    /// reported file falling under a configured `deny_paths` entry. None
    /// of these are user-facing notices — an editor integration not
    /// running, or a stale leftover file, is a normal silent state, not a
    /// misconfiguration (unlike `AGENTS.md`'s over-budget notice).
    async fn refresh_editor_context(&mut self, cwd: &Path) {
        self.editor_context_text = None;

        let Some(config) = &self.editor_context_config else {
            return;
        };
        let Some(path) = editor_context::editor_context_file_path(cwd) else {
            return;
        };
        let Some(context) = editor_context::read_editor_context(&path).await else {
            return;
        };
        if context.schema_version != editor_context::SCHEMA_VERSION {
            return;
        }
        if OffsetDateTime::now_utc() - context.updated_at > time::Duration::minutes(5) {
            return;
        }

        let canonical_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        let canonical_root = context
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| context.workspace_root.clone());
        if canonical_root != canonical_cwd {
            return;
        }

        let resolved_file = context.workspace_root.join(&context.file);
        if aivyx_sandbox::path_is_denied(&resolved_file, &config.deny_paths) {
            return;
        }

        let file_display = context.file.display();
        self.editor_context_text = Some(match &context.selection {
            None => format!(
                "Currently open in editor: {file_display}, cursor at line {}.",
                context.cursor.line
            ),
            Some(sel) => format!(
                "Currently open in editor: {file_display}, cursor at line {}, with lines \
                 {}-{} selected.",
                context.cursor.line, sel.start_line, sel.end_line
            ),
        });
    }

    /// Re-renders the map off the async runtime. Best-effort: a failure
    /// just means this turn goes without a map.
    async fn refresh_repo_map(&mut self) {
```

Note: `aivyx_sandbox::path_is_denied(&resolved_file, &config.deny_paths)` requires the visibility widening from Task 3 to compile (`path_is_denied` is currently `pub(crate)` inside `aivyx-sandbox`, not callable from `aivyx-core`). Do not attempt a workaround (local reimplementation, a different check) — leave this call exactly as written; it will start compiling once Task 3's one-line visibility change lands. If you want to confirm the rest of this task's logic compiles independently first, you may temporarily comment out just this one `if` block, run your tests, then restore it before your final commit — but the final committed state must have the real call, uncommented, exactly as shown above.

- [ ] **Step 9: Wire the refresh call into the per-turn flow**

old_string:
```
        self.refresh_repo_map().await;
        self.refresh_agents_files(cwd).await;
```

new_string:
```
        self.refresh_repo_map().await;
        self.refresh_agents_files(cwd).await;
        self.refresh_editor_context(cwd).await;
```

- [ ] **Step 10: Append `editor_context_text` in `assemble_messages`**

old_string:
```
        if let Some(text) = &self.agents_files_text {
            system.push_str("\n\n");
            system.push_str(text);
        }
        if let Some(map) = &self.repo_map_text {
            system.push_str("\n\n");
            system.push_str(map);
        }
        let mut messages = Vec::with_capacity(self.history.len() + 1);
```

new_string:
```
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
        let mut messages = Vec::with_capacity(self.history.len() + 1);
```

- [ ] **Step 11: Count `editor_context_text` in the token/compaction estimator**

This is required for correctness but not explicitly named in the spec — found during plan-writing by reading `prompt_chars`'s own doc comment ("the two must use the same measure so systematic omissions... cancel out in the ratio"): `repo_map_text` and `agents_files_text` are both already counted here; leaving `editor_context_text` out would systematically undercount prompt size by its length every turn it's present, exactly the failure mode that comment warns against.

old_string:
```
    fn prompt_chars(&self) -> usize {
        message_chars(&self.system_prompt, &self.history)
            + self.repo_map_text.as_ref().map_or(0, |m| m.chars().count())
            + self
                .agents_files_text
                .as_ref()
                .map_or(0, |m| m.chars().count())
    }
```

new_string:
```
    fn prompt_chars(&self) -> usize {
        message_chars(&self.system_prompt, &self.history)
            + self.repo_map_text.as_ref().map_or(0, |m| m.chars().count())
            + self
                .agents_files_text
                .as_ref()
                .map_or(0, |m| m.chars().count())
            + self
                .editor_context_text
                .as_ref()
                .map_or(0, |m| m.chars().count())
    }
```

- [ ] **Step 12: Write the unit tests**

`crates/aivyx-core/src/agent/tests.rs` already has the exact precedent to
follow — the `AGENTS.md` tests (search for
`global_only_agents_md_is_injected_with_no_precedence_note`,
`neither_file_present_injects_nothing`, etc.). They test through the
*public* `run_turn` API, not by calling private `refresh_*` methods
directly: construct via `build_agent(responses, ToolRegistry::new(),
max_iters) -> (Agent, UnboundedReceiver<AgentEvent>, Arc<MockBackend>)`,
call `agent.set_agents_file(...)` (here: `agent.set_editor_context(...)`),
run a turn via `agent.run_turn("hi".to_string(), dir.path(),
CancellationToken::new()).await.unwrap()`, then inspect
`mock.received.lock().unwrap()[0].messages[0].text_content()` — the exact
system-prompt text the mock backend actually received. Follow this same
shape, not a direct-method-call approach — it exercises the real,
complete per-turn flow (the actual `refresh_editor_context` call inside
`run_turn_inner`), not just the method in isolation.

Add these tests to `crates/aivyx-core/src/agent/tests.rs`, alongside the
`AGENTS.md` tests:

```rust
#[tokio::test]
async fn editor_context_not_configured_injects_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut agent, _rx, mock) = build_agent(
        vec![vec![StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        }]],
        ToolRegistry::new(),
        10,
    );

    agent
        .run_turn("hi".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    let received = mock.received.lock().unwrap();
    let system = received[0].messages[0].text_content();
    assert!(!system.contains("Currently open in editor"));
}

#[tokio::test]
async fn editor_context_surfaces_a_valid_matching_file() {
    let dir = tempfile::tempdir().unwrap();
    let canonical_dir = dir.path().canonicalize().unwrap();
    let context_path = crate::editor_context::editor_context_file_path(&canonical_dir)
        .expect("state dir should exist in tests");
    tokio::fs::create_dir_all(context_path.parent().unwrap())
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    tokio::fs::write(
        &context_path,
        format!(
            r#"{{"schema_version":1,"workspace_root":"{}","file":"src/foo.rs","cursor":{{"line":42,"column":8}},"updated_at":"{now}"}}"#,
            canonical_dir.display()
        ),
    )
    .await
    .unwrap();

    let (mut agent, _rx, mock) = build_agent(
        vec![vec![StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        }]],
        ToolRegistry::new(),
        10,
    );
    agent.set_editor_context(vec![]);

    agent
        .run_turn("hi".to_string(), &canonical_dir, CancellationToken::new())
        .await
        .unwrap();

    let received = mock.received.lock().unwrap();
    let system = received[0].messages[0].text_content();
    assert!(system.contains("Currently open in editor: src/foo.rs, cursor at line 42."));

    tokio::fs::remove_file(&context_path).await.ok();
}

#[tokio::test]
async fn editor_context_surfaces_selection_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let canonical_dir = dir.path().canonicalize().unwrap();
    let context_path = crate::editor_context::editor_context_file_path(&canonical_dir)
        .expect("state dir should exist in tests");
    tokio::fs::create_dir_all(context_path.parent().unwrap())
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    tokio::fs::write(
        &context_path,
        format!(
            r#"{{"schema_version":1,"workspace_root":"{}","file":"src/foo.rs","cursor":{{"line":42,"column":8}},"selection":{{"start_line":40,"end_line":45}},"updated_at":"{now}"}}"#,
            canonical_dir.display()
        ),
    )
    .await
    .unwrap();

    let (mut agent, _rx, mock) = build_agent(
        vec![vec![StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        }]],
        ToolRegistry::new(),
        10,
    );
    agent.set_editor_context(vec![]);

    agent
        .run_turn("hi".to_string(), &canonical_dir, CancellationToken::new())
        .await
        .unwrap();

    let received = mock.received.lock().unwrap();
    let system = received[0].messages[0].text_content();
    assert!(system.contains(
        "Currently open in editor: src/foo.rs, cursor at line 42, with lines 40-45 selected."
    ));

    tokio::fs::remove_file(&context_path).await.ok();
}

#[tokio::test]
async fn editor_context_ignores_a_stale_file() {
    let dir = tempfile::tempdir().unwrap();
    let canonical_dir = dir.path().canonicalize().unwrap();
    let context_path = crate::editor_context::editor_context_file_path(&canonical_dir)
        .expect("state dir should exist in tests");
    tokio::fs::create_dir_all(context_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(
        &context_path,
        format!(
            r#"{{"schema_version":1,"workspace_root":"{}","file":"src/foo.rs","cursor":{{"line":1,"column":1}},"updated_at":"2020-01-01T00:00:00Z"}}"#,
            canonical_dir.display()
        ),
    )
    .await
    .unwrap();

    let (mut agent, _rx, mock) = build_agent(
        vec![vec![StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        }]],
        ToolRegistry::new(),
        10,
    );
    agent.set_editor_context(vec![]);

    agent
        .run_turn("hi".to_string(), &canonical_dir, CancellationToken::new())
        .await
        .unwrap();

    let received = mock.received.lock().unwrap();
    let system = received[0].messages[0].text_content();
    assert!(!system.contains("Currently open in editor"));

    tokio::fs::remove_file(&context_path).await.ok();
}

#[tokio::test]
async fn editor_context_ignores_a_workspace_root_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();
    let canonical_dir = dir.path().canonicalize().unwrap();
    let context_path = crate::editor_context::editor_context_file_path(&canonical_dir)
        .expect("state dir should exist in tests");
    tokio::fs::create_dir_all(context_path.parent().unwrap())
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    tokio::fs::write(
        &context_path,
        format!(
            r#"{{"schema_version":1,"workspace_root":"{}","file":"src/foo.rs","cursor":{{"line":1,"column":1}},"updated_at":"{now}"}}"#,
            other_dir.path().canonicalize().unwrap().display()
        ),
    )
    .await
    .unwrap();

    let (mut agent, _rx, mock) = build_agent(
        vec![vec![StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        }]],
        ToolRegistry::new(),
        10,
    );
    agent.set_editor_context(vec![]);

    agent
        .run_turn("hi".to_string(), &canonical_dir, CancellationToken::new())
        .await
        .unwrap();

    let received = mock.received.lock().unwrap();
    let system = received[0].messages[0].text_content();
    assert!(!system.contains("Currently open in editor"));

    tokio::fs::remove_file(&context_path).await.ok();
}

#[tokio::test]
async fn editor_context_ignores_a_wrong_schema_version() {
    let dir = tempfile::tempdir().unwrap();
    let canonical_dir = dir.path().canonicalize().unwrap();
    let context_path = crate::editor_context::editor_context_file_path(&canonical_dir)
        .expect("state dir should exist in tests");
    tokio::fs::create_dir_all(context_path.parent().unwrap())
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    tokio::fs::write(
        &context_path,
        format!(
            r#"{{"schema_version":99,"workspace_root":"{}","file":"src/foo.rs","cursor":{{"line":1,"column":1}},"updated_at":"{now}"}}"#,
            canonical_dir.display()
        ),
    )
    .await
    .unwrap();

    let (mut agent, _rx, mock) = build_agent(
        vec![vec![StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        }]],
        ToolRegistry::new(),
        10,
    );
    agent.set_editor_context(vec![]);

    agent
        .run_turn("hi".to_string(), &canonical_dir, CancellationToken::new())
        .await
        .unwrap();

    let received = mock.received.lock().unwrap();
    let system = received[0].messages[0].text_content();
    assert!(!system.contains("Currently open in editor"));

    tokio::fs::remove_file(&context_path).await.ok();
}

#[tokio::test]
async fn editor_context_ignores_a_denied_path() {
    let dir = tempfile::tempdir().unwrap();
    let canonical_dir = dir.path().canonicalize().unwrap();
    let context_path = crate::editor_context::editor_context_file_path(&canonical_dir)
        .expect("state dir should exist in tests");
    tokio::fs::create_dir_all(context_path.parent().unwrap())
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    tokio::fs::write(
        &context_path,
        format!(
            r#"{{"schema_version":1,"workspace_root":"{}","file":"secret/foo.rs","cursor":{{"line":1,"column":1}},"updated_at":"{now}"}}"#,
            canonical_dir.display()
        ),
    )
    .await
    .unwrap();

    let (mut agent, _rx, mock) = build_agent(
        vec![vec![StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        }]],
        ToolRegistry::new(),
        10,
    );
    agent.set_editor_context(vec![canonical_dir.join("secret")]);

    agent
        .run_turn("hi".to_string(), &canonical_dir, CancellationToken::new())
        .await
        .unwrap();

    let received = mock.received.lock().unwrap();
    let system = received[0].messages[0].text_content();
    assert!(!system.contains("Currently open in editor"));

    tokio::fs::remove_file(&context_path).await.ok();
}

#[tokio::test]
async fn editor_context_injection_never_contains_file_content() {
    // Security regression guard for spec Decision 5: writes a real file
    // with real "secret" content on disk, points a valid context file at
    // it, runs a real turn, and confirms the actual content never reaches
    // the system prompt sent to the backend — only the path/line/column
    // metadata does. This exercises the real read-and-format path (not a
    // hand-set field), so it would actually catch a future regression
    // where someone "helpfully" adds the selected text to the injected
    // string.
    let dir = tempfile::tempdir().unwrap();
    let canonical_dir = dir.path().canonicalize().unwrap();
    std::fs::write(
        canonical_dir.join("secret.rs"),
        "fn leaked_super_secret_function() {}",
    )
    .unwrap();
    let context_path = crate::editor_context::editor_context_file_path(&canonical_dir)
        .expect("state dir should exist in tests");
    tokio::fs::create_dir_all(context_path.parent().unwrap())
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    tokio::fs::write(
        &context_path,
        format!(
            r#"{{"schema_version":1,"workspace_root":"{}","file":"secret.rs","cursor":{{"line":1,"column":1}},"updated_at":"{now}"}}"#,
            canonical_dir.display()
        ),
    )
    .await
    .unwrap();

    let (mut agent, _rx, mock) = build_agent(
        vec![vec![StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        }]],
        ToolRegistry::new(),
        10,
    );
    agent.set_editor_context(vec![]);

    agent
        .run_turn("hi".to_string(), &canonical_dir, CancellationToken::new())
        .await
        .unwrap();

    let received = mock.received.lock().unwrap();
    let system = received[0].messages[0].text_content();
    assert!(system.contains("secret.rs"));
    assert!(
        !system.contains("leaked_super_secret_function"),
        "must never inject raw file content into the system prompt"
    );

    tokio::fs::remove_file(&context_path).await.ok();
}
```

If `build_agent`'s exact signature has drifted from what's shown above
(check `fn build_agent(` in `crates/aivyx-core/src/agent/tests.rs`
directly before writing these), adjust the calls to match — the important
content is the scenarios and assertions, not this exact helper's
signature.

- [ ] **Step 13: Run the new tests**

```bash
cargo test -p aivyx-core editor_context 2>&1 | tail -40
```

Expected: all new tests pass (the module-level ones from Task 1 plus the `Agent`-level ones just added). Note: this will only fully compile and pass once Task 3's `path_is_denied` visibility widening lands (Step 8's note above) — if you're executing Task 2 before Task 3, expect a compile error at the `aivyx_sandbox::path_is_denied` call site specifically, and nothing else; if you see other, unrelated compile errors, those are real bugs in this task's own code to fix.

- [ ] **Step 14: Commit**

```bash
git add crates/aivyx-core/src/agent/types.rs crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/src/agent/tests.rs
git commit -m "Wire editor-context refresh/injection into Agent"
```

---

### Task 3: `aivyx-sandbox` visibility, `aivyx-config` setting, `main.rs` wiring

**Files:**
- Modify: `crates/aivyx-sandbox/src/lib.rs`
- Modify: `crates/aivyx-config/src/lib.rs`
- Modify: `crates/aivyx/src/main.rs`

**Interfaces:**
- Consumes: `Agent::set_editor_context(deny_paths: Vec<PathBuf>)` from Task 2 (exact signature). Completes Task 2's `aivyx_sandbox::path_is_denied` dependency (widens its visibility).
- Produces: `Settings.editor_context: EditorContextSettings { enabled: bool }` — no later task depends on this beyond `main.rs`'s own wiring in this same task.

- [ ] **Step 1: Re-verify `path_is_denied`'s exact current line/signature**

```bash
grep -n "fn path_is_denied" crates/aivyx-sandbox/src/lib.rs
```

Expected: one match, `pub(crate) fn path_is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool` (confirm the exact line before editing — it may have drifted from line 211).

- [ ] **Step 2: Widen its visibility**

Use the Edit tool on `crates/aivyx-sandbox/src/lib.rs` with the exact old_string matching what Step 1 found (adjust if it differs from below):

old_string:
```
pub(crate) fn path_is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool {
```

new_string:
```
pub fn path_is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool {
```

- [ ] **Step 3: Verify aivyx-sandbox itself still builds and its own tests pass**

```bash
cargo build -p aivyx-sandbox 2>&1 | tail -10
cargo test -p aivyx-sandbox 2>&1 | grep -E "^test result:|FAILED|error\["
```

Expected: build succeeds, all existing `aivyx-sandbox` tests still pass (a pure visibility widening changes no behavior — if anything fails here, something else is wrong, not this change).

- [ ] **Step 4: Add `EditorContextSettings` to aivyx-config**

Insert the new struct as a sibling immediately after `AgentsFileSettings`'s `Default` impl. Use the Edit tool on `crates/aivyx-config/src/lib.rs` with:

old_string:
```
impl Default for AgentsFileSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            budget_tokens: 1024,
        }
    }
}
```

new_string:
```
impl Default for AgentsFileSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            budget_tokens: 1024,
        }
    }
}

/// Live editor context (open file, cursor, selection) — see
/// `aivyx_core::editor_context` and the "Editor context" README section
/// for the JSON file contract. No budget concept (a one-line status, not
/// prose) — just an enable flag, matching `repo_map`'s own shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorContextSettings {
    pub enabled: bool,
}

impl Default for EditorContextSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}
```

If this exact old_string doesn't match (the file may have drifted since this plan was written), re-grep `impl Default for AgentsFileSettings` in `crates/aivyx-config/src/lib.rs` first to find its current exact content, then apply the same insertion immediately after its closing `}`.

- [ ] **Step 5: Add the new field to the `Settings` struct**

old_string:
```
    pub agents_file: AgentsFileSettings,
    pub web: WebSettings,
    pub mcp: McpSettings,
}
```

new_string:
```
    pub agents_file: AgentsFileSettings,
    pub editor_context: EditorContextSettings,
    pub web: WebSettings,
    pub mcp: McpSettings,
}
```

- [ ] **Step 6: Add unit tests for the new setting**

Find the existing `agents_file_settings_default_is_enabled_with_a_1024_token_budget`/`agents_file_block_parses_custom_values` tests in `crates/aivyx-config/src/lib.rs` and add these alongside them, matching their exact style:

```rust
#[test]
fn editor_context_settings_default_is_enabled() {
    let settings = Settings::default();
    assert!(settings.editor_context.enabled);
}

#[test]
fn editor_context_block_parses_custom_values() {
    let raw = r#"
        [editor_context]
        enabled = false
    "#;
    let settings: Settings = toml::from_str(raw).unwrap();
    assert!(!settings.editor_context.enabled);
}
```

- [ ] **Step 7: Run aivyx-config's tests**

```bash
cargo test -p aivyx-config 2>&1 | grep -E "^test result:|FAILED|error\["
```

Expected: all pass, including the 2 new ones.

- [ ] **Step 8: Wire `main.rs`**

Read `crates/aivyx/src/main.rs` first to re-verify the exact current line numbers around `set_agents_file` (they may have drifted from ~line 499-502), then use the Edit tool:

old_string:
```
    // Absence of either file is not an error — the feature is off only
    // when the user explicitly disables it via [agents_file] enabled.
    if settings.agents_file.enabled {
        let global_path = aivyx_config::Settings::agents_file_path().ok();
        agent.set_agents_file(global_path, settings.agents_file.budget_tokens);
    }
```

new_string:
```
    // Absence of either file is not an error — the feature is off only
    // when the user explicitly disables it via [agents_file] enabled.
    if settings.agents_file.enabled {
        let global_path = aivyx_config::Settings::agents_file_path().ok();
        agent.set_agents_file(global_path, settings.agents_file.budget_tokens);
    }

    // Absence of a context file is not an error — the feature is off only
    // when the user explicitly disables it via [editor_context] enabled.
    if settings.editor_context.enabled {
        agent.set_editor_context(deny_paths.clone());
    }
```

- [ ] **Step 9: Full workspace build and test**

```bash
cargo build --workspace 2>&1 | tail -10
cargo test --workspace 2>&1 | grep -E "^test result:|FAILED|error\["
cargo clippy --workspace --all-targets 2>&1 | tail -20
```

Expected: build succeeds (this is the point where Task 2's `path_is_denied` call site finally compiles for real, since this task's Step 2 widened its visibility), every test result reads `ok` with 0 failures, clippy stays clean.

- [ ] **Step 10: Commit**

```bash
git add crates/aivyx-sandbox/src/lib.rs crates/aivyx-config/src/lib.rs crates/aivyx/src/main.rs
git commit -m "Wire editor-context config + widen path_is_denied for cross-crate use"
```

---

### Task 4: README documentation

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: the exact JSON schema from Task 1 (`schema_version`/`workspace_root`/`file`/`cursor`/`selection`/`updated_at`) and the exact `[editor_context]` setting from Task 3 (`enabled`, default `true`).

This task depends on Tasks 1-3 being complete (it documents what they built). Before starting, re-grep `README.md` for the `## Tools` and "Configuration reference" sections to confirm current line numbers/surrounding content — this file has been edited by several prior phases in this project's history.

- [ ] **Step 1: Add an "Editor context" feature section**

Find the end of the `AGENTS.md` feature description in `README.md` (search for the paragraph ending "...review the file the same way you'd review any other project instructions before trusting them, not as a sandboxed-away concern.") and the blank line/next feature section immediately after it. Insert a new subsection there, following the same style as the existing `web_fetch`/`web_search` and MCP feature write-ups (one paragraph description, then a `Governed by [section]: ...` sentence):

Use the Edit tool with the old_string being the exact tail of the `AGENTS.md` section plus whatever begins the next section (re-grep to get this exact, since it depends on Phase 9's exact wording already in the file), and the new_string inserting this content in between:

```markdown
**Editor context**: an optional per-project JSON file
(`~/.local/state/aivyx-coder/editor-context/<hash>.json`, keyed by the same
canonicalized-`cwd` hash as session files) that any editor integration can
write to, reporting the currently open file, cursor position, and
selection. Re-read every turn and surfaced as a one-line addition to the
system prompt ("Currently open in editor: src/foo.rs, cursor at line
42.") — metadata only, never file content; the model calls `read_file`
itself for actual code, exactly as it already does everywhere else. A
file that's missing, malformed, reports an unrecognized `schema_version`,
is more than 5 minutes stale, whose `workspace_root` doesn't match this
session's own directory, or whose reported path falls under a configured
`deny_paths` entry is silently ignored — none of these are user-facing
errors. No editor plugin ships with aivyx-coder; this is the file-format
contract such a plugin (for any editor) would write to. Governed by
`[editor_context]`: `enabled` (default `true` — a no-op until some
integration actually writes the file).

The JSON schema (`schema_version: 1`):

```json
{
  "schema_version": 1,
  "workspace_root": "/abs/path/to/project",
  "file": "src/foo.rs",
  "cursor": { "line": 42, "column": 8 },
  "selection": { "start_line": 40, "end_line": 45 },
  "updated_at": "2026-07-18T12:00:00Z"
}
```

`workspace_root` is absolute and must canonicalize to aivyx-coder's own
`cwd`. `file` is relative to `workspace_root`. `cursor` is required,
1-indexed. `selection` is optional — omit the key entirely (not `null`)
when there's no active selection; 1-indexed, inclusive line range, no
column granularity in this version. `updated_at` is an RFC 3339
timestamp.
```

- [ ] **Step 2: Add the config example to the "Configuration reference" section**

Find the existing `[agents_file]`-adjacent block in the `config.toml` example (search for `[repo_map]` or `[agents_file]` inside the fenced `toml` block in the "Configuration reference" section) and add, immediately after it:

```toml
[editor_context]
enabled = true  # a no-op until some editor integration writes the context file
```

(Match the exact surrounding formatting/comment style already used for neighboring settings in that same fenced block — re-read it first rather than assuming a specific format.)

- [ ] **Step 3: Verify the edits landed correctly**

```bash
grep -n "editor_context\|Editor context" README.md
```

Expected: matches in both the new feature-description subsection and the config reference block, reading coherently in context (not cut off, no duplicated text).

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "Document editor-context integration in README"
```

---

### Task 5: Live E2E through the real binary

**Files:**
- None modified — this task only verifies, via the project's established live-E2E method (PTY + `python-pyte`, graded via the persisted session JSON).

**Interfaces:**
- Consumes: the fully-wired feature from Tasks 1-4 (already committed and merged into this branch).

- [ ] **Step 1: Build the release binary**

```bash
cargo build --release -p aivyx 2>&1 | tail -10
```

Expected: succeeds.

- [ ] **Step 2: Set up a scratch project directory and a matching editor-context file**

```bash
SCRATCH_PROJECT=$(mktemp -d)
cd "$SCRATCH_PROJECT"
git init -q  # aivyx-coder's checkpoint machinery expects a git repo; harmless here since this test makes no edits
echo 'fn calculate_total(prices: &[f64]) -> f64 { prices.iter().sum() }' > lib.rs

CANONICAL_PROJECT=$(realpath "$SCRATCH_PROJECT")
```

Compute the exact context-file path this project's own code would use — write a tiny throwaway Rust snippet or reuse a debug print, OR simplest: since the plan's own Task 1 module exposes `editor_context_file_path` as `pub(crate)` (not callable from outside the crate), write a small standalone script using the same FNV-1a algorithm (copy the exact function from Task 1's `editor_context.rs`) to compute the hash directly in this step:

```bash
python3 -c "
import sys
def fnv1a(data: bytes) -> int:
    h = 0xcbf29ce484222325
    for b in data:
        h ^= b
        h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h
print(f'{fnv1a(sys.argv[1].encode()):016x}')
" "$CANONICAL_PROJECT"
```

Use the printed hash to construct the context file path:

```bash
CONTEXT_DIR="$HOME/.local/state/aivyx-coder/editor-context"
mkdir -p "$CONTEXT_DIR"
HASH=$(python3 -c "
def fnv1a(data: bytes) -> int:
    h = 0xcbf29ce484222325
    for b in data:
        h ^= b
        h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h
print(f'{fnv1a(\"$CANONICAL_PROJECT\".encode()):016x}')
")
CONTEXT_FILE="$CONTEXT_DIR/$HASH.json"

NOW=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
cat > "$CONTEXT_FILE" << EOF
{
  "schema_version": 1,
  "workspace_root": "$CANONICAL_PROJECT",
  "file": "lib.rs",
  "cursor": { "line": 1, "column": 12 },
  "updated_at": "$NOW"
}
EOF
```

- [ ] **Step 3: Drive the real binary through a PTY and ask about the cursor**

Follow this project's established live-E2E harness pattern (see the memory/precedent from prior phases: `python-pyte` for accurate screen rendering, explicit `TIOCSWINSZ` window sizing, paced keystrokes ~15ms apart, wait for the "Type a message..." readiness marker before typing, wait for both the ready-status text AND the input-placeholder text before considering a turn complete — the "ready" text alone is not a reliable turn-complete signal). Send the message: `"What line is my cursor currently on, and in which file? Don't call any tools — just answer from what you already know."` (explicitly asking the model not to call tools isolates whether the injected context, not a `read_file` call, is what informs the answer).

Grade via the persisted session JSON (`~/.local/state/aivyx-coder/sessions/<hash>.json` for this scratch project), not raw screen text — confirm the assistant's response text mentions `lib.rs` and line `1`, and confirm no `read_file`/other tool call appears in the session's `history` for this turn (proving the answer came from the injected system-prompt context, not a tool call).

- [ ] **Step 4: Clean up**

```bash
rm -f "$CONTEXT_FILE"
rm -f ~/.local/state/aivyx-coder/sessions/*"$(basename "$SCRATCH_PROJECT")"*.json 2>/dev/null || true
rm -rf "$SCRATCH_PROJECT"
```

(The session file's own naming includes a sanitized directory-name prefix per `session::session_file_path`'s own convention — `mktemp -d`'s output directory name will appear in it; adjust the glob if needed after inspecting what file actually got created.)

- [ ] **Step 5: Report**

No commit for this task (verification only, no files modified). Report the exact session JSON excerpt confirming the assistant's answer reflected the injected context and that no tool call was needed to produce it.
