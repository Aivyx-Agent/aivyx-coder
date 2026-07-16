# `delete_file` Tool Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give aivyx-coder a dedicated `delete_file` tool — `ActionKind::Delete`'s first real constructor, closing the last item from the original tool/capability audit.

**Architecture:** One new file, `crates/aivyx-tools/src/tools/delete_file.rs`, following `write_file.rs`'s exact established `Tool` trait shape. Deletion is pure Rust (`tokio::fs::remove_file`) — no subprocess, no argv, no injection surface. Registered unconditionally in `main.rs`, right after `edit_file`.

**Tech Stack:** Rust, `tokio::fs` (already a workspace dependency). No new external crate.

## Global Constraints

- `delete_file` uses `ActionKind::Delete` + `PermissionTarget::Path` — the variant already exists in `crates/aivyx-sandbox/src/lib.rs`, no enum change needed. This is its first real constructor anywhere in the codebase.
- No `deny_paths` field on the tool — matches `write_file`/`edit_file`'s own lack of one. `ConfirmationGate::is_denied` already centrally blocks any `PermissionTarget::Path` under configured `deny_paths`.
- `permission_request` checks existence and directory-ness via `std::fs::metadata` *before* returning any `PermissionRequest` — both failure cases return `Err(ToolError::ExecutionFailed(...))`, never `InvalidArguments` (the `path` argument string itself is well-formed in both cases; the problem is a runtime precondition about what's on disk).
- Preview mirrors `write_file`'s exact pattern: file content for text, a binary-file warning for non-UTF8. No preview size cap (matching `write_file`, which has none either).
- `execute()` performs the deletion via `tokio::fs::remove_file` only — no re-check of existence/directory-ness (already established in `permission_request`, before the confirmation modal; no TOCTOU guard needed for this local, non-security-boundary operation, matching `write_file`'s own lack of any re-check in `execute()`).
- `mutates_outside_session()` is NOT overridden (trait default `true` applies) — hidden in Plan Mode, matching `write_file`/`edit_file`. No new checkpoint plumbing needed anywhere: `ToolExecutor::dispatch_inner` already checkpoints before any call where `tool.mutates_outside_session()` is true.
- No config flag — always registered, matching `write_file`/`edit_file`.
- No directory/recursive deletion in this phase — `run_shell` remains the path for that.

---

### Task 1: `delete_file` tool

**Files:**
- Create: `crates/aivyx-tools/src/tools/delete_file.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs`
- Modify: `crates/aivyx-tools/src/lib.rs`

**Interfaces:**
- Consumes: `crate::path_resolve::resolve` (existing).
- Produces: `pub struct DeleteFileTool;` (no fields, matching `WriteFileTool`). Registered by Task 2's `main.rs` wiring.

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-tools/src/tools/delete_file.rs`:

```rust
use std::path::Path;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::path_resolve::resolve;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct DeleteFileArgs {
    /// Path to the file to delete, absolute or relative to the working directory.
    path: String,
}

/// `ActionKind::Delete`'s first real constructor — every other tool in
/// this codebase declares `Read`/`Write`/`Execute`/`Internal`/`McpTool`.
/// Single-file only: a directory target is refused with a clear error
/// rather than silently doing something unexpected or requiring a
/// `recursive` flag this tool doesn't offer — `run_shell` remains the
/// path for directory removal. Deletion is pure `tokio::fs`, no
/// subprocess — no argv exists to be misparsed, unlike a `rm` shell-out.
pub struct DeleteFileTool;

#[async_trait]
impl Tool for DeleteFileTool {
    fn name(&self) -> &str {
        "delete_file"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Delete a single file. Refuses to delete directories — use run_shell \
                for that. The user sees the file's content (or a binary-file warning) and must \
                approve before it's removed."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(DeleteFileArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: DeleteFileArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(cwd, &args.path);

        let metadata = std::fs::metadata(&resolved).map_err(|_| {
            ToolError::ExecutionFailed(format!("{} does not exist", resolved.display()))
        })?;
        if metadata.is_dir() {
            return Err(ToolError::ExecutionFailed(format!(
                "{} is a directory — delete_file only removes single files; use run_shell for \
                 directory removal",
                resolved.display()
            )));
        }

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
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: DeleteFileArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(&ctx.cwd, &args.path);

        tokio::fs::remove_file(&resolved).await?;

        Ok(ToolOutput::Ok(format!("deleted {}", resolved.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn execute_deletes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gone.txt"), "bye\n").unwrap();

        let tool = DeleteFileTool;
        let output = tool
            .execute(json!({ "path": "gone.txt" }), &ctx(dir.path()))
            .await
            .unwrap();

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("deleted"), "text: {text}");
        assert!(!dir.path().join("gone.txt").exists());
    }

    #[test]
    fn permission_request_is_action_delete_with_a_path_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("target.txt"), "content\n").unwrap();

        let tool = DeleteFileTool;
        let request = tool
            .permission_request(&json!({ "path": "target.txt" }), dir.path())
            .unwrap();

        assert_eq!(request.action, ActionKind::Delete);
        let PermissionTarget::Path(path) = &request.target else {
            panic!("expected a Path target, got {:?}", request.target);
        };
        assert_eq!(path, &dir.path().join("target.txt"));
    }

    #[test]
    fn preview_shows_file_content_for_a_text_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), "important notes\n").unwrap();

        let tool = DeleteFileTool;
        let request = tool
            .permission_request(&json!({ "path": "readme.txt" }), dir.path())
            .unwrap();

        let preview = request.preview.expect("expected a preview");
        assert!(preview.contains("important notes"));
    }

    #[test]
    fn preview_warns_instead_of_showing_content_for_a_binary_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.bin"), [0xFF, 0xFE, 0x00, 0xD8, 0x00]).unwrap();

        let tool = DeleteFileTool;
        let request = tool
            .permission_request(&json!({ "path": "data.bin" }), dir.path())
            .unwrap();

        let preview = request.preview.expect("expected a warning preview, not None");
        assert!(preview.contains("WARNING"));
        assert!(preview.contains("binary"));
    }

    #[test]
    fn a_nonexistent_path_is_rejected() {
        let dir = tempfile::tempdir().unwrap();

        let tool = DeleteFileTool;
        let result = tool.permission_request(&json!({ "path": "does-not-exist.txt" }), dir.path());

        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }

    #[test]
    fn a_directory_target_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a_directory")).unwrap();

        let tool = DeleteFileTool;
        let result = tool.permission_request(&json!({ "path": "a_directory" }), dir.path());

        let Err(ToolError::ExecutionFailed(message)) = result else {
            panic!("expected ExecutionFailed, got {result:?}");
        };
        assert!(message.contains("directory"), "message: {message}");
    }
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p aivyx-tools delete_file`
Expected: FAIL to compile (`delete_file` module not yet wired into `tools/mod.rs`).

- [ ] **Step 3: Wire the module in**

In `crates/aivyx-tools/src/tools/mod.rs`, add `mod delete_file;` as the very first line (alphabetically first among the `mod` declarations — "delete_file" sorts before "edit_file"):

```rust
mod delete_file;
mod edit_file;
mod find_references;
...
```

and `pub use delete_file::DeleteFileTool;` as the very first line of the `pub use` block:

```rust
pub use delete_file::DeleteFileTool;
pub use edit_file::EditFileTool;
pub use find_references::FindReferencesTool;
...
```

In `crates/aivyx-tools/src/lib.rs`'s `pub use tools::{ ... };` block, add `DeleteFileTool` alphabetically (it sorts before `EditFileTool`):

```rust
pub use tools::{
    DeleteFileTool, EditFileTool, FindReferencesTool, GetMcpPromptTool, GitBranchTool,
    GitCommitTool, GitPrTool, GitPushTool, GitReadTool, GlobTool, GoToDefinitionTool, GrepTool,
    ListMcpPromptsTool, ListMcpResourcesTool, McpToolAdapter, ReadFileTool, ReadMcpResourceTool,
    RunCommandTool, RunShellTool, SetTasksTool, WebFetchTool, WebSearchTool, WriteFileTool,
};
```

- [ ] **Step 4: Run to confirm it passes**

Run: `cargo test -p aivyx-tools delete_file::`
Expected: PASS — 6 tests (`execute_deletes_the_file`, `permission_request_is_action_delete_with_a_path_target`, `preview_shows_file_content_for_a_text_file`, `preview_warns_instead_of_showing_content_for_a_binary_file`, `a_nonexistent_path_is_rejected`, `a_directory_target_is_rejected`).

- [ ] **Step 5: Run the whole crate's tests and clippy**

Run: `cargo test -p aivyx-tools`
Run: `cargo clippy -p aivyx-tools --all-targets`
Expected: all pass, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/tools/delete_file.rs crates/aivyx-tools/src/tools/mod.rs crates/aivyx-tools/src/lib.rs
git commit -m "ActionKind::Delete cleanup: delete_file tool (Task 1)"
```

---

### Task 2: `main.rs` wiring

**Files:**
- Modify: `crates/aivyx/src/main.rs`

**Interfaces:**
- Consumes: `aivyx_tools::DeleteFileTool` (Task 1).
- Produces: nothing further downstream — this is the final integration point.

- [ ] **Step 1: Extend the import list**

In `crates/aivyx/src/main.rs`, the current `aivyx_tools::{...}` import reads:

```rust
use aivyx_tools::{
    CommandSpec, EditFileTool, FindReferencesTool, GetMcpPromptTool, GitBranchTool,
    GitCheckpointer, GitCommitTool, GitPrTool, GitPushTool, GitReadTool, GlobTool,
    GoToDefinitionTool, GrepTool, ListMcpPromptsTool, ListMcpResourcesTool, LspClient, McpClient,
    McpToolAdapter, ReadFileTool, ReadMcpResourceTool, RunCommandTool, RunShellTool, SetTasksTool,
    ToolExecutor, ToolRegistry, WebFetchTool, WebSearchTool, WriteFileTool,
};
```

Add `DeleteFileTool` alphabetically (it sorts between `CommandSpec` and `EditFileTool`):

```rust
use aivyx_tools::{
    CommandSpec, DeleteFileTool, EditFileTool, FindReferencesTool, GetMcpPromptTool,
    GitBranchTool, GitCheckpointer, GitCommitTool, GitPrTool, GitPushTool, GitReadTool, GlobTool,
    GoToDefinitionTool, GrepTool, ListMcpPromptsTool, ListMcpResourcesTool, LspClient, McpClient,
    McpToolAdapter, ReadFileTool, ReadMcpResourceTool, RunCommandTool, RunShellTool, SetTasksTool,
    ToolExecutor, ToolRegistry, WebFetchTool, WebSearchTool, WriteFileTool,
};
```

(If the real current list at execution time differs from this — later phases may have touched it — insert `DeleteFileTool` alphabetically into whatever the actual list is; don't blindly overwrite an import list that's drifted from this example.)

- [ ] **Step 2: Register the tool**

In `crates/aivyx/src/main.rs`, find the existing three-line block:

```rust
    registry.register(Arc::new(ReadFileTool));
    registry.register(Arc::new(WriteFileTool));
    registry.register(Arc::new(EditFileTool));
```

and add immediately after it, unconditionally (no config flag, matching these three):

```rust
    registry.register(Arc::new(DeleteFileTool));
```

- [ ] **Step 3: Build**

Run: `cargo build --workspace`
Expected: clean build.

- [ ] **Step 4: Manual smoke check**

There is no way to unit-test `main.rs`'s own wiring in isolation (matching this project's established precedent for wiring tasks). Run the built binary in a real scratch directory (not this project's own repo) and confirm, via a quick manual conversation: asking the model to delete a file it can see shows a confirmation modal naming the file's content in the preview, and approving it genuinely removes the file from disk.

- [ ] **Step 5: Run the full workspace test suite and clippy**

Run: `cargo test --workspace`
Run: `cargo clippy --workspace --all-targets`
Expected: all tests pass (no new automated tests in this task — wiring-only, matching every prior phase's own main.rs task), clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx/src/main.rs
git commit -m "ActionKind::Delete cleanup: wire delete_file into main.rs (Task 2)"
```

---

### Task 3: Docs + live E2E + final check

**Files:**
- Modify: `README.md`
- Modify: `ROADMAP.md`

**Interfaces:**
- Consumes: the complete feature from Tasks 1–2.

- [ ] **Step 1: Live E2E through the real binary**

Using the same PTY-harness pattern as the most recent phases (a `python-pyte`-driven PTY session, verifying results via the persisted session JSON rather than raw screen text). Set up a scratch git repository (an initialized repo with at least one commit, so the existing `GitCheckpointer` is active — check via `settings.git.checkpoints`, default `true`, and that the scratch directory is a real git repo, which `GitCheckpointer::detect` requires).

This phase's live E2E covers new ground beyond every prior phase's "real tool call, real confirmation modal" checks: it must also confirm the automatic pre-delete checkpoint genuinely lets the deleted file be restored — something no earlier phase's live E2E has specifically exercised (checkpoints existed before this phase, but this is the first time a live E2E deletes something and then proves it back via checkpoint restore).

Confirm, through the real TUI:
1. A direct message asking the model to delete a specific file — the tool call and result appear live in the transcript, **with a confirmation modal** showing the file's content in the preview (matching `ActionKind::Delete`'s confirm-gated tier), and the file is genuinely gone from disk afterward (verified via the persisted session JSON, per this project's established grading method, and by checking the scratch directory directly).
2. Find the checkpoint ref the deletion created: `git for-each-ref refs/aivyx/checkpoints/` in the scratch repo, taking the most recent one (or use `aivyx_core::Agent::latest_checkpoint_ref`-equivalent behavior — for this manual live-E2E step, inspecting refs directly via `git for-each-ref` is sufficient, no need to drive this through the agent itself). Confirm `git checkout <that-ref> -- <deleted-file-path>` in the scratch repo genuinely restores the file with its original content — this is the direct payoff of the "checkpoints are the safety net, not bespoke recovery machinery" design decision, and the one part of this phase's live E2E that's genuinely new ground rather than a repeat of prior phases' own checks.

- [ ] **Step 2: Update `README.md`**

Add a new paragraph after the branch/PR tooling paragraph, documenting: `delete_file`, `ActionKind::Delete` being exercised for the first time in the codebase, the preview showing file content (or a binary-file warning), single-file-only scope (directories refused, `run_shell` remains the path for those), the pure-Rust `tokio::fs::remove_file` implementation (no subprocess, no argv-injection surface — a deliberate contrast with the git-tool phase's recent dash-injection lesson), and the automatic pre-delete checkpoint as the recovery mechanism (`git checkout <checkpoint-ref> -- <path>` to restore a deleted file), rather than any bespoke undo feature.

- [ ] **Step 3: Update `ROADMAP.md`**

In the Phase 9 section, insert a "built and live-verified" paragraph (matching the style of the entries immediately above it), covering: this closing out the original tool/capability audit's last remaining item; `ActionKind::Delete` finally getting its first real constructor; the deliberate choice of pure Rust filesystem calls over shelling out to `rm`, explicitly framed as sidestepping the exact argv-injection vulnerability class the branch/PR tooling phase just found and fixed in `git_branch`/`git_push` (there's no argv to construct at all); the decision not to add any bespoke recovery/confirmation-richness mechanism, deferring entirely to the existing automatic pre-mutation checkpoint (consistent with this project's established pattern, e.g. enforced verification's own retry-exhaustion path relying on the same mechanism); the single-file-only scope; the test count delta; and the live E2E results, specifically calling out the checkpoint-restore verification as new ground beyond prior phases' own live E2E checks.

- [ ] **Step 4: Final full-workspace check**

Run: `cargo test --workspace`
Expected: all tests pass. State the actual counted new-test delta in the docs (Task 1: 6, Task 2–3: 0 — 6 new tests total on top of whatever the baseline is at the time this task runs), following this project's own precedent of correcting any projected count against the real one rather than trusting the plan's estimate.

Run: `cargo clippy --workspace --all-targets`
Expected: clean, no warnings.

- [ ] **Step 5: Commit**

```bash
git add README.md ROADMAP.md
git commit -m "Docs: delete_file tool + live E2E verification (Phase 9)"
```
