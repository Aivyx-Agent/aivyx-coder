# Editor Approval Integration ("Context Out") Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user's editor answer a pending `ConfirmationGate` permission
decision (write/edit/delete/execute/MCP-tool) as a second, equally-trusted
surface racing the terminal's own prompt — first decision wins, editor
absent means unchanged terminal-only behavior.

**Architecture:** `ConfirmationGate::check` (in `aivyx-sandbox`) races the
existing terminal `prompter.prompt` future against a new polling future in a
`tokio::select!`. The polling future writes a pending-request JSON file
describing the decision (diff content for writes/edits/deletes, command text
for shell execs, description text for MCP tools), then polls for a matching
response file. Both files live under the same per-project, hash-keyed state
directory convention `session.rs`/`editor_context.rs` already established.

**Tech Stack:** Rust, tokio (`select!`, `fs`, `time`), serde/serde_json,
`uuid` (new dependency — see Global Constraints).

## Global Constraints

- **Corrected module placement (deviates from the spec's literal text — see
  rationale below):** the spec's Change 3 says the new schema/path-keying
  module lives in `crates/aivyx-core/src/editor_approval.rs`. This is
  **wrong** and must not be followed literally: `aivyx-sandbox`'s
  `Cargo.toml` has zero dependency on `aivyx-core` (verified directly —
  `aivyx-sandbox`'s only dependencies are `async-trait`, `landlock`,
  `libc`, `seccompiler`, `serde_json`, `tokio`, `tracing`), while
  `aivyx-core` depends on `aivyx-sandbox`. Since `ConfirmationGate::check`
  (which needs to call this module) lives in `aivyx-sandbox`, the module
  must live in `crates/aivyx-sandbox/src/editor_approval.rs` instead — the
  reverse placement would require `aivyx-sandbox` to depend on
  `aivyx-core`, creating a dependency cycle. This also means `Agent` never
  touches editor-approval at all (unlike `editor_context`, which needed
  `Agent::refresh_editor_context`/`assemble_messages`) — the spec's own
  Change 4 already suspected this ("most likely a constructor parameter…
  directly [on `ConfirmationGate`] rather than routing through `Agent`");
  this plan confirms it and drops the spec's `EditorApprovalConfig` (in
  `agent/types.rs`) entirely, since nothing in `aivyx-core` needs it.
- **New dependencies, `crates/aivyx-sandbox/Cargo.toml`:** `directories =
  "6.0.0"` (state-dir resolution, matching `aivyx-core`'s version), `serde
  = { version = "1.0.228", features = ["derive"] }` (matching
  `aivyx-core`'s version — `aivyx-sandbox` already depends on `serde_json`
  but not bare `serde`), `uuid = { version = "1", features = ["v4"] }`
  (new to the whole workspace — needed for `request_id` generation, spec
  Change 3 calls for "a fresh UUID per request"). `tokio`'s existing
  dependency line gains features: currently `features = ["process"]`,
  needs `"fs"` (async file read/write) and `"time"` (poll-loop sleep) and
  `"macros"` (the `tokio::select!` macro — currently only present in
  `[dev-dependencies]`'s `tokio`, needed in the main dependency too).
- **`request_id` format:** a `uuid::Uuid::new_v4()`, `.to_string()`'d —
  matches the spec's literal "UUID" requirement exactly (not a shortcut
  hex-timestamp scheme).
- **Poll interval:** the spec assumes an existing "editor-context polling
  interval" to reuse — this doesn't exist. `editor_context`'s own refresh
  is a fire-and-forget check once per agent turn (`refresh_editor_context`
  called from `run_turn_inner`), not a sleep-loop poll. This plan
  introduces a genuinely new constant,
  `const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);`,
  in the new module — fast enough to feel responsive, slow enough not to
  busy-loop, with no existing precedent to match instead.
- **`PermissionRequest`/`ConfirmationGate::new` are widely constructed
  across the workspace** (42 `PermissionRequest { .. }` literals: 23 in
  `aivyx-tools/src` incl. tests, 17 in `aivyx-sandbox/src/confirmation.rs`
  tests, 1 in `aivyx-core/src/delegate.rs`, 1 in
  `aivyx-core/src/agent/tests.rs`; 18 `ConfirmationGate::new(..)` call
  sites: 17 in `confirmation.rs` tests + 1 in `crates/aivyx/src/main.rs`).
  Every task touching these signatures must fix ALL call sites, driven by
  `cargo build`'s own "missing field"/"this function takes N arguments"
  errors rather than manual line-number enumeration (line numbers will
  have drifted by execution time) — a clean `cargo build --workspace
  --all-targets` is the completeness proof for these mechanical fixups,
  not a manual grep count.
- **`ActionKind`'s six variants**: `Read | Write | Execute | Delete |
  Internal | McpTool`. Only `Write | Execute | Delete | McpTool` ever
  reach the interactive-prompt tier (`Read`/`Internal` auto-allow earlier;
  autonomous mode never prompts at all, for any action). This plan's new
  logic only ever needs to handle these four kinds.
- **`PermissionTarget`**: `Path(PathBuf) | Command { program: String,
  args: Vec<String> } | Other(String)`.
- Existing tier order in `ConfirmationGate::check` (deny_paths hard block
  → Read/Internal auto-allow → plan-mode deny → autonomous-mode resolution
  → Always-Allow cache → interactive prompt) must not change. This plan's
  race logic replaces exactly one line: the final
  `self.prompter.prompt(request).await` call, and nothing before it.
- Full test suite (`cargo test --workspace`) and `cargo clippy --workspace
  --all-targets` must stay clean (0 failures, 0 warnings) after every task.

---

### Task 1: `PermissionRequest` gains a `diff` field

**Files:**
- Modify: `crates/aivyx-sandbox/src/lib.rs` (the `PermissionRequest` struct)
- Modify: `crates/aivyx-tools/src/tools/write_file.rs`
- Modify: `crates/aivyx-tools/src/tools/edit_file.rs`
- Modify: `crates/aivyx-tools/src/tools/delete_file.rs`
- Modify (mechanical, compiler-driven): every other file listed in Global
  Constraints containing a `PermissionRequest { .. }` literal

**Interfaces:**
- Produces: `pub struct DiffContent { pub old_content: String, pub
  new_content: String }` (in `aivyx-sandbox/src/lib.rs`, `#[derive(Debug,
  Clone)]` to match `PermissionRequest`'s own derives) and the new
  `PermissionRequest.diff: Option<DiffContent>` field. Later tasks (Task 2,
  Task 3) read `request.diff` to build editor-approval content.

- [ ] **Step 1: Add `DiffContent` and the `diff` field**

In `crates/aivyx-sandbox/src/lib.rs`, find the `PermissionRequest` struct
(currently):

```rust
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub tool_name: String,
    pub action: ActionKind,
    pub target: PermissionTarget,
    pub arguments_preview: serde_json::Value,
    /// Human-renderable preview of the effect (e.g. a unified diff for a
    /// file write/edit). Computed by the tool, since only it has the old
    /// and new content — this crate and the UI treat it as an opaque string.
    pub preview: Option<String>,
}
```

Replace with:

```rust
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub tool_name: String,
    pub action: ActionKind,
    pub target: PermissionTarget,
    pub arguments_preview: serde_json::Value,
    /// Human-renderable preview of the effect (e.g. a unified diff for a
    /// file write/edit). Computed by the tool, since only it has the old
    /// and new content — this crate and the UI treat it as an opaque string.
    pub preview: Option<String>,
    /// Structured before/after content for a file write/edit/delete, for
    /// consumers (e.g. an editor-approval integration) that want to render
    /// their own native diff view rather than a preformatted text blob.
    /// `None` when there's no meaningful structured content (a non-file
    /// action, or a file whose content can't be read as text — see
    /// `write_file`'s/`delete_file`'s own binary-file fallback).
    pub diff: Option<DiffContent>,
}

/// Structured before/after text for a file write/edit/delete.
/// `old_content` is empty for a brand-new file (nothing existed before).
#[derive(Debug, Clone)]
pub struct DiffContent {
    pub old_content: String,
    pub new_content: String,
}
```

- [ ] **Step 2: Populate `diff` in `write_file.rs`**

In `crates/aivyx-tools/src/tools/write_file.rs`, find:

```rust
        let preview = match std::fs::read_to_string(&resolved) {
            Ok(old) => Some(unified_diff(
                &resolved.display().to_string(),
                &old,
                &args.content,
            )),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(unified_diff(
                &resolved.display().to_string(),
                "",
                &args.content,
            )),
            // Fails open on the gate decision (the user can still approve
            // or deny) but must NOT look identical to the new-file case —
            // an existing binary/non-UTF8 file is about to be destroyed.
            Err(_) => Some(format!(
                "WARNING: {} already exists but could not be read as text (binary file?). \
                 This write will overwrite it entirely.",
                resolved.display()
            )),
        };

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(resolved),
            arguments_preview: json!({ "path": args.path }),
            preview,
        })
```

Replace with (adds `diff`, computed alongside `preview` from the same
match — `None` in the binary-file branch, since there's no text content to
hand a diff viewer):

```rust
        let (preview, diff) = match std::fs::read_to_string(&resolved) {
            Ok(old) => (
                Some(unified_diff(&resolved.display().to_string(), &old, &args.content)),
                Some(DiffContent { old_content: old, new_content: args.content.clone() }),
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (
                Some(unified_diff(&resolved.display().to_string(), "", &args.content)),
                Some(DiffContent { old_content: String::new(), new_content: args.content.clone() }),
            ),
            // Fails open on the gate decision (the user can still approve
            // or deny) but must NOT look identical to the new-file case —
            // an existing binary/non-UTF8 file is about to be destroyed.
            // No structured diff either: there's no text content to hand a
            // diff viewer.
            Err(_) => (
                Some(format!(
                    "WARNING: {} already exists but could not be read as text (binary file?). \
                     This write will overwrite it entirely.",
                    resolved.display()
                )),
                None,
            ),
        };

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(resolved),
            arguments_preview: json!({ "path": args.path }),
            preview,
            diff,
        })
```

Add `DiffContent` to the existing import line near the top of the file
(currently `use aivyx_sandbox::{ActionKind, PermissionRequest,
PermissionTarget};`):

```rust
use aivyx_sandbox::{ActionKind, DiffContent, PermissionRequest, PermissionTarget};
```

- [ ] **Step 3: Add a test for the new-file `diff` population**

Add to `write_file.rs`'s existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn new_file_diff_has_empty_old_content() {
        let dir = tempfile::tempdir().unwrap();
        let args = serde_json::json!({ "path": "new.txt", "content": "hello\n" });

        let request = WriteFileTool.permission_request(&args, dir.path()).unwrap();

        let diff = request.diff.expect("expected diff content for a new file");
        assert_eq!(diff.old_content, "");
        assert_eq!(diff.new_content, "hello\n");
    }

    #[test]
    fn existing_binary_file_has_no_structured_diff() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("existing.bin");
        std::fs::write(&target, [0xFF, 0xFE, 0x00, 0xD8, 0x00]).unwrap();
        let args = serde_json::json!({ "path": "existing.bin", "content": "hello\n" });

        let request = WriteFileTool.permission_request(&args, dir.path()).unwrap();

        assert!(request.diff.is_none(), "a binary file has no text diff content");
    }
```

Adding the required `diff` field to the struct in Step 1 means every
other `PermissionRequest { .. }` literal in the workspace is now a compile
error until it's given a `diff` value too — this won't build cleanly
until Steps 4-6 (below) finish that mechanical fixup, so don't try to run
`cargo test` yet.

- [ ] **Step 4: Populate `diff` in `edit_file.rs`**

In `crates/aivyx-tools/src/tools/edit_file.rs`, find:

```rust
        let preview = Some(unified_diff(
            &resolved.display().to_string(),
            &old_content,
            &new_content,
        ));

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(resolved),
            arguments_preview: json!({ "path": args.path, "replace_all": args.replace_all }),
            preview,
        })
```

Replace with:

```rust
        let preview = Some(unified_diff(
            &resolved.display().to_string(),
            &old_content,
            &new_content,
        ));
        let diff = Some(DiffContent {
            old_content: old_content.clone(),
            new_content: new_content.clone(),
        });

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(resolved),
            arguments_preview: json!({ "path": args.path, "replace_all": args.replace_all }),
            preview,
            diff,
        })
```

Update the import line (find `use aivyx_sandbox::{ActionKind,
PermissionRequest, PermissionTarget};` near the top of the file):

```rust
use aivyx_sandbox::{ActionKind, DiffContent, PermissionRequest, PermissionTarget};
```

Add a test to `edit_file.rs`'s existing `#[cfg(test)] mod tests` block
(read the file first to match its existing test helper style — it
already has a way to construct args/call `permission_request`, follow
that exact pattern):

```rust
    #[test]
    fn diff_carries_the_real_before_and_after_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
        let args = serde_json::json!({
            "path": "a.rs",
            "old_string": "fn foo() {}",
            "new_string": "fn foo() -> i32 { 42 }",
        });

        let request = EditFileTool.permission_request(&args, dir.path()).unwrap();

        let diff = request.diff.expect("expected diff content");
        assert_eq!(diff.old_content, "fn foo() {}\n");
        assert_eq!(diff.new_content, "fn foo() -> i32 { 42 }\n");
    }
```

- [ ] **Step 5: Populate `diff` in `delete_file.rs`**

In `crates/aivyx-tools/src/tools/delete_file.rs`, find:

```rust
        let preview = match std::fs::read_to_string(&resolved) {
            Ok(content) => Some(content),
            Err(_) => Some(format!(
                "WARNING: {} could not be read as text (binary file?). This will delete it \
                 entirely.",
                resolved.display()
            )),
        };

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Delete,
            target: PermissionTarget::Path(resolved),
            arguments_preview: json!({ "path": args.path }),
            preview,
        })
```

Replace with (`new_content` is always empty for a deletion — the file
will cease to exist):

```rust
        let (preview, diff) = match std::fs::read_to_string(&resolved) {
            Ok(content) => (
                Some(content.clone()),
                Some(DiffContent { old_content: content, new_content: String::new() }),
            ),
            Err(_) => (
                Some(format!(
                    "WARNING: {} could not be read as text (binary file?). This will delete it \
                     entirely.",
                    resolved.display()
                )),
                None,
            ),
        };

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Delete,
            target: PermissionTarget::Path(resolved),
            arguments_preview: json!({ "path": args.path }),
            preview,
            diff,
        })
```

Update the import line (`use aivyx_sandbox::{ActionKind,
PermissionRequest, PermissionTarget};` → add `DiffContent`):

```rust
use aivyx_sandbox::{ActionKind, DiffContent, PermissionRequest, PermissionTarget};
```

Add a test to `delete_file.rs`'s existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn diff_carries_old_content_with_empty_new_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), "important notes\n").unwrap();

        let tool = DeleteFileTool;
        let request = tool
            .permission_request(&json!({ "path": "readme.txt" }), dir.path())
            .unwrap();

        let diff = request.diff.expect("expected diff content for a text file");
        assert_eq!(diff.old_content, "important notes\n");
        assert_eq!(diff.new_content, "");
    }

    #[test]
    fn binary_file_has_no_structured_diff() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.bin"), [0xFF, 0xFE, 0x00, 0xD8, 0x00]).unwrap();

        let tool = DeleteFileTool;
        let request = tool
            .permission_request(&json!({ "path": "data.bin" }), dir.path())
            .unwrap();

        assert!(request.diff.is_none(), "a binary file has no text diff content");
    }
```

- [ ] **Step 6: Fix every remaining `PermissionRequest { .. }` literal, driven by the compiler**

Run: `cargo build --workspace --all-targets 2>&1 | grep -A3 "missing field"`

This lists every remaining struct literal missing the new `diff` field —
in `aivyx-tools/src/tools/{git_read,find_references,web_search,read_file,
mcp_meta,git_branch,git_commit,grep,set_tasks,glob,run_command,git_push,
git_pr,web_fetch,run_shell,go_to_definition,mcp_tool}.rs` (both their
non-test construction sites and any test-only literals in the same
files), `aivyx-sandbox/src/confirmation.rs`'s test module (17 literals,
via its `write_request`/`read_request` helpers plus several inline
literals — read the file to find each), `aivyx-core/src/delegate.rs`, and
`aivyx-core/src/agent/tests.rs`. For every one of these (none of them are
write/edit/delete actions, so none need real diff content), add a single
line `diff: None,` immediately after the existing `preview: None,` (or
wherever `preview` sits) in that literal. Repeat `cargo build --workspace
--all-targets` and fix the next reported site until the command exits
clean. A clean build IS the completeness proof here — a missing field is
a hard compile error, not a silent gap, so there is no need to separately
cross-check a count of edited sites.

Expected final output: `cargo build --workspace --all-targets` exits 0
with no errors.

- [ ] **Step 7: Run the full test suite**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: every line reads `ok. N passed; 0 failed` — the pre-existing
suite is unaffected (none of the ~39 mechanical `diff: None` sites changed
any assertion), plus the 4 new tests from Steps 3, 4, 5 pass.

- [ ] **Step 8: Clippy**

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "Add PermissionRequest.diff field for structured before/after content"
```

---

### Task 2: `editor_approval` module in `aivyx-sandbox`

**Files:**
- Create: `crates/aivyx-sandbox/src/editor_approval.rs`
- Modify: `crates/aivyx-sandbox/src/lib.rs` (declare the new module)
- Modify: `crates/aivyx-sandbox/Cargo.toml` (new dependencies)

**Interfaces:**
- Consumes: `PermissionRequest`, `ActionKind`, `PermissionTarget`,
  `DiffContent`, `UserResponse` (all from `crate::` — same crate, `lib.rs`)
  from Task 1.
- Produces (all `pub(crate)`, consumed by Task 3):
  - `pub(crate) fn request_path(cwd: &Path) -> Option<PathBuf>`
  - `pub(crate) fn response_path(cwd: &Path) -> Option<PathBuf>`
  - `pub(crate) struct PendingApprovalRequest { .. }` (see Step 2)
  - `pub(crate) fn build_pending_request(request: &PermissionRequest) ->
    Option<PendingApprovalRequest>`
  - `pub(crate) async fn write_pending_request(path: &Path, pending:
    &PendingApprovalRequest) -> std::io::Result<()>`
  - `pub(crate) async fn poll_for_response(path: &Path, expected_request_id:
    &str) -> UserResponse`
  - `pub(crate) const POLL_INTERVAL: std::time::Duration`

- [ ] **Step 1: Add new dependencies**

In `crates/aivyx-sandbox/Cargo.toml`, change:

```toml
[dependencies]
async-trait = "0.1.89"
landlock = { version = "0.4.5", optional = true }
libc = { version = "0.2.186", optional = true }
seccompiler = { version = "0.5.0", optional = true }
serde_json = "1.0.150"
tokio = { version = "1.52.3", features = ["process"] }
tracing = "0.1.44"
```

to:

```toml
[dependencies]
async-trait = "0.1.89"
directories = "6.0.0"
landlock = { version = "0.4.5", optional = true }
libc = { version = "0.2.186", optional = true }
seccompiler = { version = "0.5.0", optional = true }
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.150"
tokio = { version = "1.52.3", features = ["fs", "macros", "process", "time"] }
tracing = "0.1.44"
uuid = { version = "1", features = ["v4"] }
```

Run: `cargo build -p aivyx-sandbox 2>&1 | tail -10`
Expected: succeeds (no code changes yet, just new deps available).

- [ ] **Step 2: Write the module — schema types and path keying**

Create `crates/aivyx-sandbox/src/editor_approval.rs`:

```rust
//! Editor-side approval of a pending `ConfirmationGate` decision: a small,
//! versioned JSON contract that lets an editor integration answer the same
//! decision the terminal's own confirmation modal is waiting on. See
//! `docs/superpowers/specs/2026-07-19-editor-approval-integration-design.md`
//! for the full design.
//!
//! Lives in `aivyx-sandbox` (not `aivyx-core`, unlike the sibling
//! `editor_context` module in `aivyx-core`) because `ConfirmationGate`,
//! the only consumer, lives here — `aivyx-sandbox` has no dependency on
//! `aivyx-core`, so the module can't live on the other side of that edge.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{ActionKind, PermissionRequest, PermissionTarget, UserResponse};

/// Bumped if the on-disk shape changes incompatibly. A response file
/// reporting any other value is treated as absent — this is a
/// machine-to-machine contract, not a human-edited config file.
const SCHEMA_VERSION: u32 = 1;

/// How often `poll_for_response` re-reads the response file while waiting.
/// No existing polling-interval constant to reuse in this codebase —
/// `editor_context`'s own refresh is a once-per-turn check, not a
/// sleep-loop. Fast enough to feel responsive, slow enough not to busy-loop.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The action-kind-specific content of a pending request — a tagged union
/// serializing to exactly the shape described in the design spec's Change 3
/// (`action_kind` field plus whichever content fields that kind carries).
#[derive(Debug, Serialize)]
#[serde(tag = "action_kind", rename_all = "snake_case")]
pub(crate) enum ApprovalContent {
    Write {
        old_content: String,
        new_content: String,
    },
    Delete {
        old_content: String,
        will_delete: bool,
    },
    Execute {
        command: String,
        args: Vec<String>,
    },
    McpTool {
        description: String,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct PendingApprovalRequest {
    pub(crate) schema_version: u32,
    pub(crate) request_id: String,
    pub(crate) target: String,
    #[serde(flatten)]
    pub(crate) content: ApprovalContent,
}

#[derive(Debug, Deserialize)]
struct ApprovalResponse {
    schema_version: u32,
    request_id: String,
    decision: Decision,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Allow,
    Deny,
    AlwaysAllow,
}

/// Where an editor integration writes/reads pending-approval files for this
/// project: `~/.local/state/aivyx-coder/editor-approval/` on Linux, keyed
/// by a stable hash of the canonicalized `cwd` — identical construction to
/// `editor_context_file_path` in `aivyx-core` (own inlined FNV-1a copy,
/// same rationale: the key must be stable across program versions, and
/// `std`'s `DefaultHasher` doesn't guarantee that; this crate can't import
/// `aivyx-core`'s copy since the dependency points the other way).
fn state_subdir_path(cwd: &Path, filename_suffix: &str) -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "aivyx-coder")?;
    let state_dir = dirs
        .state_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dirs.data_local_dir().to_path_buf());

    let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let key = format!("{:016x}", fnv1a(canonical.to_string_lossy().as_bytes()));
    Some(
        state_dir
            .join("editor-approval")
            .join(format!("{key}-{filename_suffix}.json")),
    )
}

pub(crate) fn request_path(cwd: &Path) -> Option<PathBuf> {
    state_subdir_path(cwd, "request")
}

pub(crate) fn response_path(cwd: &Path) -> Option<PathBuf> {
    state_subdir_path(cwd, "response")
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
```

- [ ] **Step 3: Write `build_pending_request`**

Append to the same file:

```rust
/// Builds the content to write for a pending request, or `None` if this
/// request either can't reach the interactive-prompt tier (`Read`/
/// `Internal` — `ConfirmationGate` never calls this for those) or has no
/// structured content to offer the editor at all (a `Write`/`Delete`
/// action whose `diff` is `None` — e.g. `write_file`'s/`delete_file`'s own
/// binary-file fallback, which has no text content to hand a diff viewer).
/// `None` here means the editor-approval channel simply doesn't
/// participate for this one request — the terminal remains the sole
/// surface, exactly like "no editor connected" behaves.
pub(crate) fn build_pending_request(
    request: &PermissionRequest,
    request_id: String,
) -> Option<PendingApprovalRequest> {
    let target = match &request.target {
        PermissionTarget::Path(path) => path.display().to_string(),
        PermissionTarget::Command { program, args } => format!("{program} {}", args.join(" ")),
        PermissionTarget::Other(description) => description.clone(),
    };

    let content = match request.action {
        ActionKind::Write => {
            let diff = request.diff.as_ref()?;
            ApprovalContent::Write {
                old_content: diff.old_content.clone(),
                new_content: diff.new_content.clone(),
            }
        }
        ActionKind::Delete => {
            let diff = request.diff.as_ref()?;
            ApprovalContent::Delete {
                old_content: diff.old_content.clone(),
                will_delete: true,
            }
        }
        ActionKind::Execute => {
            let PermissionTarget::Command { program, args } = &request.target else {
                return None;
            };
            ApprovalContent::Execute {
                command: program.clone(),
                args: args.clone(),
            }
        }
        ActionKind::McpTool => ApprovalContent::McpTool {
            description: request.preview.clone().unwrap_or_else(|| target.clone()),
        },
        ActionKind::Read | ActionKind::Internal => return None,
    };

    Some(PendingApprovalRequest {
        schema_version: SCHEMA_VERSION,
        request_id,
        target,
        content,
    })
}
```

- [ ] **Step 4: Write `write_pending_request` and `poll_for_response`**

Append to the same file:

```rust
/// Writes the pending-request file, 0600 (explicit, not relying on the
/// process umask — this file can carry real file content or command text,
/// unlike `editor_context`'s deliberately metadata-only file).
pub(crate) async fn write_pending_request(
    path: &Path,
    pending: &PendingApprovalRequest,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let json = serde_json::to_string_pretty(pending)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;
    tokio::fs::write(path, json).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    Ok(())
}

/// Reads and parses the response file at `path`. `None` on any I/O error,
/// malformed JSON, or a `schema_version` this build doesn't recognize — a
/// missing/leftover/incompatible file is a normal, silent state.
async fn read_response(path: &Path) -> Option<ApprovalResponse> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    let response: ApprovalResponse = serde_json::from_str(&content).ok()?;
    if response.schema_version != SCHEMA_VERSION {
        return None;
    }
    Some(response)
}

/// Polls `path` every `POLL_INTERVAL` until a response whose `request_id`
/// matches `expected_request_id` appears, then returns the corresponding
/// `UserResponse`. Never returns for a non-matching or absent response —
/// intended to be raced against the terminal's own prompt via
/// `tokio::select!`, which drops whichever branch loses.
pub(crate) async fn poll_for_response(path: &Path, expected_request_id: &str) -> UserResponse {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        if let Some(response) = read_response(path).await
            && response.request_id == expected_request_id
        {
            return match response.decision {
                Decision::Allow => UserResponse::Allow,
                Decision::Deny => UserResponse::Deny,
                Decision::AlwaysAllow => UserResponse::AllowAlways,
            };
        }
    }
}
```

- [ ] **Step 5: Declare the module**

In `crates/aivyx-sandbox/src/lib.rs`, find:

```rust
#[cfg(feature = "sandbox-backend")]
mod confiner;
mod confirmation;
```

Replace with:

```rust
#[cfg(feature = "sandbox-backend")]
mod confiner;
mod confirmation;
mod editor_approval;
```

(No `pub use` needed — everything Task 3 uses is `pub(crate)`, consumed
directly via `crate::editor_approval::..` from `confirmation.rs`, same
crate.)

- [ ] **Step 6: Write module tests**

Append a `#[cfg(test)] mod tests` block to
`crates/aivyx-sandbox/src/editor_approval.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::DiffContent;

    #[test]
    fn same_directory_keys_to_the_same_request_and_response_paths() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        let a1 = request_path(dir_a.path()).expect("state dir should exist in tests");
        let a2 = request_path(dir_a.path()).unwrap();
        let b = request_path(dir_b.path()).unwrap();

        assert_eq!(a1, a2, "same directory must key to the same request file");
        assert_ne!(a1, b, "different directories must not collide");
        assert_ne!(
            request_path(dir_a.path()).unwrap(),
            response_path(dir_a.path()).unwrap(),
            "request and response paths must differ"
        );
        assert!(a1.to_string_lossy().contains("editor-approval"));
    }

    fn write_request(path: &str, diff: Option<DiffContent>) -> PermissionRequest {
        PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(PathBuf::from(path)),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff,
        }
    }

    #[test]
    fn builds_write_content_from_diff() {
        let request = write_request(
            "/project/a.rs",
            Some(DiffContent {
                old_content: "old\n".to_string(),
                new_content: "new\n".to_string(),
            }),
        );

        let pending = build_pending_request(&request, "req-1".to_string())
            .expect("expected pending content for a write with diff");
        assert_eq!(pending.target, "/project/a.rs");
        let ApprovalContent::Write { old_content, new_content } = pending.content else {
            panic!("expected Write content");
        };
        assert_eq!(old_content, "old\n");
        assert_eq!(new_content, "new\n");
    }

    #[test]
    fn write_with_no_diff_produces_no_pending_content() {
        let request = write_request("/project/a.bin", None);
        assert!(
            build_pending_request(&request, "req-1".to_string()).is_none(),
            "no structured diff means no editor-approval participation for this request"
        );
    }

    #[test]
    fn builds_delete_content_with_will_delete_true() {
        let request = PermissionRequest {
            tool_name: "delete_file".to_string(),
            action: ActionKind::Delete,
            target: PermissionTarget::Path(PathBuf::from("/project/gone.txt")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: Some(DiffContent {
                old_content: "bye\n".to_string(),
                new_content: String::new(),
            }),
        };

        let pending = build_pending_request(&request, "req-1".to_string()).unwrap();
        let ApprovalContent::Delete { old_content, will_delete } = pending.content else {
            panic!("expected Delete content");
        };
        assert_eq!(old_content, "bye\n");
        assert!(will_delete);
    }

    #[test]
    fn builds_execute_content_from_command_target() {
        let request = PermissionRequest {
            tool_name: "run_command".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "cargo".to_string(),
                args: vec!["test".to_string()],
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };

        let pending = build_pending_request(&request, "req-1".to_string()).unwrap();
        assert_eq!(pending.target, "cargo test");
        let ApprovalContent::Execute { command, args } = pending.content else {
            panic!("expected Execute content");
        };
        assert_eq!(command, "cargo");
        assert_eq!(args, vec!["test".to_string()]);
    }

    #[test]
    fn builds_mcp_tool_content_from_preview() {
        let request = PermissionRequest {
            tool_name: "mcp__filesystem__search".to_string(),
            action: ActionKind::McpTool,
            target: PermissionTarget::Other("search (server: filesystem)".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: Some("search_docs(query=\"foo\")".to_string()),
            diff: None,
        };

        let pending = build_pending_request(&request, "req-1".to_string()).unwrap();
        let ApprovalContent::McpTool { description } = pending.content else {
            panic!("expected McpTool content");
        };
        assert_eq!(description, "search_docs(query=\"foo\")");
    }

    #[test]
    fn read_and_internal_actions_produce_no_pending_content() {
        let read_request = PermissionRequest {
            tool_name: "read_file".to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Path(PathBuf::from("/project/a.rs")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        assert!(build_pending_request(&read_request, "req-1".to_string()).is_none());
    }

    #[tokio::test]
    async fn write_then_read_round_trips_the_response() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("response.json");
        tokio::fs::write(
            &path,
            r#"{ "schema_version": 1, "request_id": "abc", "decision": "allow" }"#,
        )
        .await
        .unwrap();

        let response = read_response(&path).await.expect("should parse");
        assert_eq!(response.request_id, "abc");
        assert_eq!(response.decision, Decision::Allow);
    }

    #[tokio::test]
    async fn wrong_schema_version_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("response.json");
        tokio::fs::write(
            &path,
            r#"{ "schema_version": 99, "request_id": "abc", "decision": "allow" }"#,
        )
        .await
        .unwrap();

        assert!(read_response(&path).await.is_none());
    }

    #[tokio::test]
    async fn poll_for_response_ignores_a_mismatched_request_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("response.json");
        tokio::fs::write(
            &path,
            r#"{ "schema_version": 1, "request_id": "other-request", "decision": "allow" }"#,
        )
        .await
        .unwrap();

        let result = tokio::time::timeout(
            Duration::from_millis(600),
            poll_for_response(&path, "expected-request"),
        )
        .await;

        assert!(
            result.is_err(),
            "a mismatched request_id must never resolve the poll"
        );
    }

    #[tokio::test]
    async fn poll_for_response_returns_deny_and_always_allow_correctly() {
        let dir = tempfile::tempdir().unwrap();

        let deny_path = dir.path().join("deny.json");
        tokio::fs::write(
            &deny_path,
            r#"{ "schema_version": 1, "request_id": "r1", "decision": "deny" }"#,
        )
        .await
        .unwrap();
        assert_eq!(
            poll_for_response(&deny_path, "r1").await,
            UserResponse::Deny
        );

        let always_path = dir.path().join("always.json");
        tokio::fs::write(
            &always_path,
            r#"{ "schema_version": 1, "request_id": "r2", "decision": "always_allow" }"#,
        )
        .await
        .unwrap();
        assert_eq!(
            poll_for_response(&always_path, "r2").await,
            UserResponse::AllowAlways
        );
    }

    #[tokio::test]
    async fn write_pending_request_creates_a_readable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("request.json");
        let pending = PendingApprovalRequest {
            schema_version: SCHEMA_VERSION,
            request_id: "req-1".to_string(),
            target: "/project/a.rs".to_string(),
            content: ApprovalContent::Write {
                old_content: "old\n".to_string(),
                new_content: "new\n".to_string(),
            },
        };

        write_pending_request(&path, &pending).await.unwrap();

        let written = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(written.contains("\"action_kind\":\"write\""));
        assert!(written.contains("req-1"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = tokio::fs::metadata(&path).await.unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "request file must be 0600");
        }
    }
}
```

- [ ] **Step 7: Run the new tests**

Run: `cargo test -p aivyx-sandbox editor_approval -- --nocapture`
Expected: all new tests pass.

- [ ] **Step 8: Run the full workspace suite and clippy**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: all `ok`, 0 failed.

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "Add editor_approval module: schema, path keying, request/response I/O"
```

---

### Task 3: `ConfirmationGate::check` races the editor-approval response

**Files:**
- Modify: `crates/aivyx-sandbox/src/confirmation.rs`

**Interfaces:**
- Consumes: `editor_approval::{request_path, response_path,
  build_pending_request, write_pending_request, poll_for_response}` (Task
  2), `uuid::Uuid` (Task 2's new dependency).
- Produces: `ConfirmationGate::new` gains a 7th constructor parameter,
  `editor_approval_enabled: bool` — Task 4 wires this from config.

- [ ] **Step 1: Add the new constructor parameter and field**

In `crates/aivyx-sandbox/src/confirmation.rs`, find the struct and
constructor:

```rust
pub struct ConfirmationGate {
    prompter: Arc<dyn PermissionPrompter>,
    deny_paths: Vec<PathBuf>,
    plan_mode: PlanMode,
    autonomous_mode: AutonomousMode,
    cwd: PathBuf,
    always_allow: Mutex<HashSet<PermissionKey>>,
}

impl ConfirmationGate {
    /// `pre_approved_commands` seeds the Always-Allow cache directly (as
    /// `(program, args)` pairs) rather than requiring an interactive
    /// confirmation the first time — these represent commands the user
    /// already trusted by writing them into config, so an additional
    /// "are you sure" click adds friction without a security benefit. This
    /// is the "command-level allowlisting" trust tier: `run_command` only
    /// ever runs entries from this same list, and `run_shell` treats an
    /// exact match against it as pre-approved before falling back to the
    /// normal confirm-then-cache flow for anything else.
    pub fn new(
        prompter: Arc<dyn PermissionPrompter>,
        deny_paths: Vec<PathBuf>,
        pre_approved_commands: Vec<(String, Vec<String>)>,
        plan_mode: PlanMode,
        autonomous_mode: AutonomousMode,
        cwd: PathBuf,
    ) -> Self {
        let always_allow = pre_approved_commands
            .into_iter()
            .map(|(program, args)| PermissionKey::Command { program, args })
            .collect();
        Self {
            prompter,
            deny_paths,
            plan_mode,
            autonomous_mode,
            cwd,
            always_allow: Mutex::new(always_allow),
        }
    }
```

Replace with:

```rust
pub struct ConfirmationGate {
    prompter: Arc<dyn PermissionPrompter>,
    deny_paths: Vec<PathBuf>,
    plan_mode: PlanMode,
    autonomous_mode: AutonomousMode,
    cwd: PathBuf,
    always_allow: Mutex<HashSet<PermissionKey>>,
    editor_approval_enabled: bool,
}

impl ConfirmationGate {
    /// `pre_approved_commands` seeds the Always-Allow cache directly (as
    /// `(program, args)` pairs) rather than requiring an interactive
    /// confirmation the first time — these represent commands the user
    /// already trusted by writing them into config, so an additional
    /// "are you sure" click adds friction without a security benefit. This
    /// is the "command-level allowlisting" trust tier: `run_command` only
    /// ever runs entries from this same list, and `run_shell` treats an
    /// exact match against it as pre-approved before falling back to the
    /// normal confirm-then-cache flow for anything else.
    ///
    /// `editor_approval_enabled` gates the editor-side answer race in
    /// `check` below (`docs/superpowers/specs/
    /// 2026-07-19-editor-approval-integration-design.md`) — when `false`,
    /// `check` behaves exactly as it did before this feature existed.
    pub fn new(
        prompter: Arc<dyn PermissionPrompter>,
        deny_paths: Vec<PathBuf>,
        pre_approved_commands: Vec<(String, Vec<String>)>,
        plan_mode: PlanMode,
        autonomous_mode: AutonomousMode,
        cwd: PathBuf,
        editor_approval_enabled: bool,
    ) -> Self {
        let always_allow = pre_approved_commands
            .into_iter()
            .map(|(program, args)| PermissionKey::Command { program, args })
            .collect();
        Self {
            prompter,
            deny_paths,
            plan_mode,
            autonomous_mode,
            cwd,
            always_allow: Mutex::new(always_allow),
            editor_approval_enabled,
        }
    }
```

- [ ] **Step 2: Replace the terminal-only prompt with the race**

Find, in `check`:

```rust
        let key = PermissionKey::from_request(request);
        if self.always_allow.lock().unwrap().contains(&key) {
            tracing::info!(
                tool = %request.tool_name,
                action = ?request.action,
                target = ?request.target,
                "permission allowed (cached Always-Allow)"
            );
            return PermissionDecision::AllowAlways;
        }

        let decision = match self.prompter.prompt(request).await {
```

Replace the `let decision = match self.prompter.prompt(request).await {`
line only, with:

```rust
        let decision = match self.resolve_via_prompter_or_editor(request).await {
```

(Everything after that line — the `UserResponse::Allow => ...`,
`AllowAlways => ...`, `Deny => ...` arms and the closing `};` — stays
byte-for-byte unchanged. This is the one line being replaced; the
Always-Allow cache insertion inside the `AllowAlways` arm automatically
covers an editor-side Always-Allow too, since both paths now produce the
identical `UserResponse::AllowAlways` value.)

- [ ] **Step 3: Add `resolve_via_prompter_or_editor`**

Add this new method to `impl ConfirmationGate` (the inherent-methods
block containing `is_denied`/`is_outside_autonomous_worktree`, not the
`impl PermissionGate for ConfirmationGate` trait block):

```rust
    /// Races the terminal's own prompt against a possible editor-side
    /// answer to the same pending request. When editor-approval is
    /// disabled, or there's no structured content to offer the editor for
    /// this particular request (e.g. a binary-file diff gap), falls back
    /// to the terminal path unchanged — exactly as `check` behaved before
    /// this feature existed.
    async fn resolve_via_prompter_or_editor(&self, request: &PermissionRequest) -> UserResponse {
        if !self.editor_approval_enabled {
            return self.prompter.prompt(request).await;
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let Some(pending) = editor_approval::build_pending_request(request, request_id.clone())
        else {
            return self.prompter.prompt(request).await;
        };
        let (Some(req_path), Some(resp_path)) = (
            editor_approval::request_path(&self.cwd),
            editor_approval::response_path(&self.cwd),
        ) else {
            return self.prompter.prompt(request).await;
        };

        if editor_approval::write_pending_request(&req_path, &pending)
            .await
            .is_err()
        {
            return self.prompter.prompt(request).await;
        }

        let response: UserResponse = tokio::select! {
            response = self.prompter.prompt(request) => response,
            response = editor_approval::poll_for_response(&resp_path, &request_id) => response,
        };

        let _ = tokio::fs::remove_file(&req_path).await;
        let _ = tokio::fs::remove_file(&resp_path).await;

        response
    }
```

Add the new import at the top of the file (find the existing `use
crate::{ ... };` block):

```rust
use crate::{
    ActionKind, AutonomousMode, PermissionDecision, PermissionGate, PermissionPrompter,
    PermissionRequest, PermissionTarget, PlanMode, UserResponse, editor_approval, path_is_denied,
};
```

- [ ] **Step 4: Fix every existing `ConfirmationGate::new(..)` call site in this file's tests**

Run: `cargo build -p aivyx-sandbox --tests 2>&1 | grep -B2 "this function takes 7 arguments"`

This lists every one of the 17 pre-existing `ConfirmationGate::new(..)`
calls in this file's `#[cfg(test)] mod tests` block, each currently ending
with `PathBuf::from("/home/user/project"),` and a closing `);`. Add
`false,` as the new 7th argument to each (these legacy tests don't
exercise editor-approval racing, so `false` preserves their exact prior
behavior). Repeat the build command until it exits clean.

- [ ] **Step 5: Add tests proving the race actually works**

Append to the existing `#[cfg(test)] mod tests` block in
`confirmation.rs`:

```rust
    /// A prompter that never resolves — used to prove the editor-response
    /// branch of the race can win deterministically, without any timing
    /// dependency on how fast a "normal" prompter would answer.
    struct NeverPrompter;

    #[async_trait]
    impl PermissionPrompter for NeverPrompter {
        async fn prompt(&self, _request: &PermissionRequest) -> UserResponse {
            std::future::pending::<()>().await;
            unreachable!("NeverPrompter must never resolve")
        }
    }

    #[tokio::test]
    async fn editor_response_wins_when_terminal_never_answers() {
        let cwd_dir = tempfile::tempdir().unwrap();
        let gate = ConfirmationGate::new(
            Arc::new(NeverPrompter),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            cwd_dir.path().to_path_buf(),
            true,
        );

        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(cwd_dir.path().join("a.rs")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: Some(crate::DiffContent {
                old_content: "old\n".to_string(),
                new_content: "new\n".to_string(),
            }),
        };

        // Poll the request file until it appears, extract the request_id
        // the gate generated, then answer it — mirroring what a real
        // editor plugin would do.
        let req_path = editor_approval::request_path(cwd_dir.path()).unwrap();
        let resp_path = editor_approval::response_path(cwd_dir.path()).unwrap();
        let answer_task = tokio::spawn(async move {
            let request_id = loop {
                if let Ok(content) = tokio::fs::read_to_string(&req_path).await {
                    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
                    break value["request_id"].as_str().unwrap().to_string();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            };
            let response_json = format!(
                r#"{{ "schema_version": 1, "request_id": "{request_id}", "decision": "allow" }}"#
            );
            tokio::fs::write(&resp_path, response_json).await.unwrap();
        });

        let decision = gate.check(&request).await;
        answer_task.await.unwrap();

        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[tokio::test]
    async fn terminal_still_answers_when_editor_approval_is_disabled() {
        let cwd_dir = tempfile::tempdir().unwrap();
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            cwd_dir.path().to_path_buf(),
            false,
        );

        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(cwd_dir.path().join("a.rs")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: Some(crate::DiffContent {
                old_content: "old\n".to_string(),
                new_content: "new\n".to_string(),
            }),
        };

        let decision = gate.check(&request).await;
        assert_eq!(decision, PermissionDecision::Allow);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

        // No pending-request file should ever have been written.
        let req_path = editor_approval::request_path(cwd_dir.path()).unwrap();
        assert!(!req_path.exists());
    }

    #[tokio::test]
    async fn no_structured_diff_falls_back_to_the_terminal_even_when_enabled() {
        let cwd_dir = tempfile::tempdir().unwrap();
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            cwd_dir.path().to_path_buf(),
            true,
        );

        // A Write action with diff: None (the binary-file gap) has no
        // pending content to offer the editor at all.
        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(cwd_dir.path().join("a.bin")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };

        let decision = gate.check(&request).await;
        assert!(matches!(decision, PermissionDecision::Deny(_)));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn always_allow_from_the_editor_populates_the_same_cache() {
        let cwd_dir = tempfile::tempdir().unwrap();
        let gate = ConfirmationGate::new(
            Arc::new(NeverPrompter),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            cwd_dir.path().to_path_buf(),
            true,
        );

        let target_path = cwd_dir.path().join("a.rs");
        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(target_path.clone()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: Some(crate::DiffContent {
                old_content: "old\n".to_string(),
                new_content: "new\n".to_string(),
            }),
        };

        let req_path = editor_approval::request_path(cwd_dir.path()).unwrap();
        let resp_path = editor_approval::response_path(cwd_dir.path()).unwrap();
        let answer_task = tokio::spawn(async move {
            let request_id = loop {
                if let Ok(content) = tokio::fs::read_to_string(&req_path).await {
                    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
                    break value["request_id"].as_str().unwrap().to_string();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            };
            let response_json = format!(
                r#"{{ "schema_version": 1, "request_id": "{request_id}", "decision": "always_allow" }}"#
            );
            tokio::fs::write(&resp_path, response_json).await.unwrap();
        });

        let decision = gate.check(&request).await;
        answer_task.await.unwrap();
        assert_eq!(decision, PermissionDecision::AllowAlways);

        // Second identical request must now hit the Always-Allow cache
        // without writing a new pending-request file at all.
        let decision2 = gate.check(&request).await;
        assert_eq!(decision2, PermissionDecision::AllowAlways);
        assert!(
            !editor_approval::request_path(cwd_dir.path()).unwrap().exists(),
            "cached Always-Allow must short-circuit before ever reaching the editor race"
        );
    }

    #[tokio::test]
    async fn both_files_are_deleted_after_resolution() {
        let cwd_dir = tempfile::tempdir().unwrap();
        let gate = ConfirmationGate::new(
            Arc::new(NeverPrompter),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            cwd_dir.path().to_path_buf(),
            true,
        );

        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(cwd_dir.path().join("a.rs")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: Some(crate::DiffContent {
                old_content: "old\n".to_string(),
                new_content: "new\n".to_string(),
            }),
        };

        let req_path = editor_approval::request_path(cwd_dir.path()).unwrap();
        let resp_path = editor_approval::response_path(cwd_dir.path()).unwrap();
        let resp_path_for_task = resp_path.clone();
        let req_path_for_task = req_path.clone();
        let answer_task = tokio::spawn(async move {
            let request_id = loop {
                if let Ok(content) = tokio::fs::read_to_string(&req_path_for_task).await {
                    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
                    break value["request_id"].as_str().unwrap().to_string();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            };
            let response_json = format!(
                r#"{{ "schema_version": 1, "request_id": "{request_id}", "decision": "deny" }}"#
            );
            tokio::fs::write(&resp_path_for_task, response_json).await.unwrap();
        });

        gate.check(&request).await;
        answer_task.await.unwrap();

        assert!(!req_path.exists(), "request file must be deleted after resolution");
        assert!(!resp_path.exists(), "response file must be deleted after resolution");
    }
```

- [ ] **Step 6: Run the new tests**

Run: `cargo test -p aivyx-sandbox confirmation:: -- --nocapture --test-threads=1`

(`--test-threads=1` avoids any two of this file's tests racing on the
same tempdir-hashed global-state-dir files if their tempdir paths ever
happened to collide — they won't in practice, since each uses its own
fresh `tempfile::tempdir()`, but serializing this one file's test run is
cheap insurance for a suite this size and removes any doubt.)

Expected: all pass, including the 5 new tests.

- [ ] **Step 7: Run the full workspace suite and clippy**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: all `ok`, 0 failed.

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "ConfirmationGate::check races editor-side approval against the terminal prompt"
```

---

### Task 4: `EditorApprovalSettings` + `main.rs` wiring

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs`
- Modify: `crates/aivyx/src/main.rs`

**Interfaces:**
- Consumes: `ConfirmationGate::new`'s new 7th parameter (Task 3).
- Produces: `settings.editor_approval.enabled: bool` (default `true`),
  read once at startup in `main.rs`.

- [ ] **Step 1: Add `EditorApprovalSettings`**

In `crates/aivyx-config/src/lib.rs`, find `EditorContextSettings`:

```rust
pub struct EditorContextSettings {
    pub enabled: bool,
}

impl Default for EditorContextSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}
```

Add immediately after it:

```rust
/// Gates whether `ConfirmationGate` will race a pending permission
/// decision against a possible editor-side answer (see
/// `docs/superpowers/specs/2026-07-19-editor-approval-integration-design.md`).
/// Unlike `EditorContextSettings`, there is no `deny_paths` concept here —
/// `deny_paths` is already enforced upstream of `ConfirmationGate` ever
/// reaching the interactive-prompt tier for a denied target at all.
/// Defaults to `true`, same as `editor_context`: the feature is inert
/// without an active external process writing a response file, so
/// `enabled` alone grants no new capability.
pub struct EditorApprovalSettings {
    pub enabled: bool,
}

impl Default for EditorApprovalSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}
```

Find the `Settings` struct's field list (containing `pub editor_context:
EditorContextSettings,`):

```rust
    pub editor_context: EditorContextSettings,
    pub web: WebSettings,
```

Replace with:

```rust
    pub editor_context: EditorContextSettings,
    pub editor_approval: EditorApprovalSettings,
    pub web: WebSettings,
```

- [ ] **Step 2: Add a config test**

Find `EditorContextSettings`'s own existing test in
`crates/aivyx-config/src/lib.rs` (search for a test name containing
`editor_context_settings_default`) and add an equivalent immediately
after it, matching that test's exact style:

```rust
    #[test]
    fn editor_approval_settings_default_is_enabled() {
        assert!(EditorApprovalSettings::default().enabled);
    }
```

- [ ] **Step 3: Run the config crate's tests**

Run: `cargo test -p aivyx-config 2>&1 | grep -E "^test result|FAILED"`
Expected: `ok`, 0 failed (including the new test, plus any existing
"deserializes with all fields defaulted" style test that constructs a
full `Settings` from empty/partial TOML — that test should keep passing
unchanged, proving `#[serde(default)]` on `Settings` already covers the
new field the same way it covers every other section).

- [ ] **Step 4: Wire `main.rs`**

In `crates/aivyx/src/main.rs`, find:

```rust
    let (prompter, permission_rx) = aivyx_tui::permission_channel();
    let gate: Arc<dyn PermissionGate> = Arc::new(ConfirmationGate::new(
        Arc::new(prompter),
        deny_paths.clone(),
        pre_approved_commands,
        plan_mode.clone(),
        autonomous_mode.clone(),
        cwd.clone(),
    ));
```

Replace with:

```rust
    let (prompter, permission_rx) = aivyx_tui::permission_channel();
    let gate: Arc<dyn PermissionGate> = Arc::new(ConfirmationGate::new(
        Arc::new(prompter),
        deny_paths.clone(),
        pre_approved_commands,
        plan_mode.clone(),
        autonomous_mode.clone(),
        cwd.clone(),
        settings.editor_approval.enabled,
    ));
```

- [ ] **Step 5: Build and test the whole workspace**

Run: `cargo build --workspace --all-targets 2>&1 | tail -10`
Expected: succeeds.

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: all `ok`, 0 failed.

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "Add EditorApprovalSettings and wire it into ConfirmationGate::new"
```

---

### Task 5: README documentation

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: the shipped behavior from Tasks 1-4 (no code changes in this
  task).

- [ ] **Step 1: Locate the existing "Editor context" section**

Run: `grep -n "^### Editor context\|^\[editor_context\]" README.md`

This should report the "Editor context" feature subsection (added by
editor-context-integration, between the `AGENTS.md` section and
`web_fetch`/`web_search`) and the `[editor_context]` config block (in the
Configuration reference, immediately after `[repo_map]`). Re-verify these
still exist at roughly the locations described, since exact line numbers
may have drifted since that phase merged.

- [ ] **Step 2: Add a new "Editor approval" feature subsection**

Immediately after the existing "Editor context" subsection (before
`web_fetch`/`web_search`), add:

```markdown
**Editor approval**: a follow-on to editor context — lets the user's
editor answer a pending permission decision (a file write/edit/delete, a
shell command, or an MCP tool call) instead of requiring a terminal
keypress. When enabled and a pending decision is raised, aivyx-coder
writes a request file describing it (the real before/after file content
for a write/edit/delete, or the command text for a shell/MCP action) to
`~/.local/state/aivyx-coder/editor-approval/<hash>-request.json`, then
waits on either the terminal's own Allow/Deny/Always-Allow prompt or a
matching `~/.local/state/aivyx-coder/editor-approval/<hash>-response.json`
file — whichever answers first wins; the other is dropped. Both files are
created 0600 and deleted the instant the decision resolves, whichever
surface answered. No editor integration ships in this repo for any
specific editor — this is a schema contract (see below) an editor plugin
implements against, exactly like editor context. Governed by
`[editor_approval]`: `enabled` (default `true` — inert without an
external process actually writing a response file).

Request file schema:

```json
{
  "schema_version": 1,
  "request_id": "6a6e...-uuid",
  "action_kind": "write",
  "target": "src/foo.rs",
  "old_content": "fn foo() {}\n",
  "new_content": "fn foo() -> i32 { 42 }\n"
}
```

`action_kind` is one of `write`, `delete`, `execute`, `mcp_tool`, each
with different content fields: `write` carries `old_content`/`new_content`
(old empty for a brand-new file); `delete` carries `old_content` plus
`will_delete: true` (no `new_content` key at all); `execute` carries
`command`/`args`; `mcp_tool` carries a `description` string. A `write` or
`delete` request whose underlying file can't be read as text (a binary
file) never generates a request file at all — the terminal remains the
sole surface for that one decision, same as when no editor integration is
running.

Response file schema (written by the editor integration):

```json
{
  "schema_version": 1,
  "request_id": "6a6e...-uuid",
  "decision": "allow"
}
```

`decision` is one of `allow`, `deny`, `always_allow` — `always_allow`
feeds the exact same Always-Allow cache a terminal Always-Allow does,
keyed on the same exact target. A response whose `request_id` doesn't
match the currently pending request is ignored.
```

- [ ] **Step 3: Add the config reference block**

Find the `[editor_context]` block in the Configuration reference section
and add immediately after it:

```markdown
```toml
[editor_approval]
enabled = true
```
```

- [ ] **Step 4: Verify accuracy against the real code**

Run: `grep -n "action_kind\|will_delete\|POLL_INTERVAL" crates/aivyx-sandbox/src/editor_approval.rs`

Confirm the field names/values documented in Step 2 match exactly what
the module actually serializes (they should, since Step 2's text was
written directly from Task 2's code above — this is a final sanity check,
not expected to find a mismatch).

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "Document editor approval integration in README"
```

---

### Task 6: Live E2E through the real binary

**Files:**
- None modified — verification only, via this project's established
  live-E2E method (PTY + `python-pyte`, graded via the persisted session
  JSON).

**Interfaces:**
- Consumes: the fully-wired feature from Tasks 1-5.

- [ ] **Step 1: Build the release binary**

```bash
cargo build --release -p aivyx 2>&1 | tail -10
```

Expected: succeeds.

- [ ] **Step 2: Set up a scratch project directory**

```bash
SCRATCH_PROJECT=$(mktemp -d)
cd "$SCRATCH_PROJECT"
git init -q
echo 'fn calculate_total(prices: &[f64]) -> f64 { prices.iter().sum() }' > lib.rs
CANONICAL_PROJECT=$(realpath "$SCRATCH_PROJECT")
```

- [ ] **Step 3: Compute the request/response file paths**

Same FNV-1a algorithm as `crates/aivyx-sandbox/src/editor_approval.rs`'s
`fnv1a` (identical to the one used for editor-context in the prior
phase's live E2E):

```bash
HASH=$(python3 -c "
def fnv1a(data: bytes) -> int:
    h = 0xcbf29ce484222325
    for b in data:
        h ^= b
        h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h
print(f'{fnv1a(\"$CANONICAL_PROJECT\".encode()):016x}')
")
APPROVAL_DIR="$HOME/.local/state/aivyx-coder/editor-approval"
REQUEST_FILE="$APPROVAL_DIR/$HASH-request.json"
RESPONSE_FILE="$APPROVAL_DIR/$HASH-response.json"
```

- [ ] **Step 4: Drive the real binary through a PTY, trigger a write, answer from the "editor"**

Follow this project's established live-E2E harness pattern (`python-pyte`
for rendering, explicit `TIOCSWINSZ` window sizing, paced keystrokes
~15ms apart, wait for the "Type a message..." readiness marker, wait for
both the ready-status text and the input-placeholder text before
considering a turn complete — see the memory/precedent from prior
phases). Send a message asking the model to write a new file, e.g.:
`"Create a file called greeting.rs containing a function that returns the string 'hello'. Use write_file."`

Once the pending confirmation is raised (the TUI shows its own
Allow/Deny/Always-Allow modal, and `$REQUEST_FILE` now exists on disk —
poll for the file's existence with a short timeout rather than a fixed
sleep), read `request_id` out of it and write `$RESPONSE_FILE`:

```bash
python3 -c "
import json, sys
with open('$REQUEST_FILE') as f:
    req = json.load(f)
with open('$RESPONSE_FILE', 'w') as f:
    json.dump({'schema_version': 1, 'request_id': req['request_id'], 'decision': 'allow'}, f)
"
```

**Without pressing any key in the terminal**, wait for the turn to
complete (same readiness-marker wait as any other turn) and confirm:
- `greeting.rs` now exists in `$SCRATCH_PROJECT` with the expected content
  (proving the write actually proceeded from the editor's decision alone).
- Both `$REQUEST_FILE` and `$RESPONSE_FILE` are gone afterward (deleted by
  `ConfirmationGate` on resolution).
- The persisted session JSON (`~/.local/state/aivyx-coder/sessions/`,
  same directory-name-prefixed key as the project) shows the `write_file`
  tool call's result as a success, not a denial — confirming the decision
  that reached the agent really was `Allow`, not some fallback.

- [ ] **Step 5: Clean up**

```bash
rm -f "$REQUEST_FILE" "$RESPONSE_FILE"
rm -f ~/.local/state/aivyx-coder/sessions/*"$(basename "$SCRATCH_PROJECT")"*.json 2>/dev/null || true
rm -rf "$SCRATCH_PROJECT"
```

(Adjust the session-file glob after inspecting what file actually got
created, per `session::session_file_path`'s own naming convention — same
caveat as the editor-context phase's own live E2E.)

- [ ] **Step 6: Report**

No commit for this task (verification only, no files modified). Report
the exact confirmation showing `greeting.rs`'s content, the session
JSON's tool-result excerpt, and confirmation both editor-approval files
were gone after resolution.
