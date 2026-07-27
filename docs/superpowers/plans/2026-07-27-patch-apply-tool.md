# Patch-Apply Tool Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `patch_file` tool that applies a unified-diff patch to an existing file, closing the second-to-last item in the 2026-07-22 capability-audit backlog.

**Architecture:** A single new tool in `aivyx-tools`, mirroring `edit_file.rs`'s exact shape (read → transform → write, dry-run for the preview). No new `ActionKind`/`PermissionTarget` — reuses `ActionKind::Write` + `PermissionTarget::Path`, so nothing in `aivyx-sandbox`, `aivyx-tui`, or `aivyx-acp` changes. Patch parsing/application is delegated to the `diffy` crate (new dependency) for its built-in fuzzy hunk-position matching.

**Tech Stack:** Rust, the `diffy` crate (0.5.1, default features — no `std`/`color`/`binary` feature needed for this usage).

## Global Constraints

- Every `cargo test` invocation MUST include `-- --test-threads=1` (aivyx-sandbox's confiner tests hang under default parallelism in this sandboxed dev environment — unrelated to this feature's code, but this plan's Task 1 touches `aivyx-tools`, which is unaffected either way; included here for consistency with every other command in this project's plans).
- Full spec: `docs/superpowers/specs/2026-07-27-patch-apply-tool-design.md`. Three resolved decisions that shape Task 1: (1) one file per call, matching every other file tool; (2) the preview/diff is recomputed from real applied content, never an echo of the input patch text; (3) the target file must already exist — no new-file or whole-file-delete support (use `write_file`/`delete_file` for those).
- `diffy::Patch::from_str` is an **inherent** method (not the stdlib `FromStr` trait — its signature borrows from the input with a lifetime the trait can't express), so it's called as `diffy::Patch::from_str(s)` directly, no `use std::str::FromStr` needed.
- A critical, verified-against-real-`diffy`-0.5.1 behavior every step below assumes: text with **no** recognizable diff syntax at all (pure prose, not even an `@@` hunk header) does **not** produce a parse error from `Patch::from_str` — it parses successfully as an empty (zero-hunk) patch, and applying zero hunks returns the original content unchanged. This means garbage input is caught by the **no-op guard**, not a dedicated "malformed patch" error path. A genuine `ParsePatchError` only occurs for text that looks like it's attempting diff syntax but gets it wrong (e.g. a `@@ ... @@` line that isn't a valid hunk header, or a hunk header whose declared line count doesn't match the lines that follow it).

---

### Task 1: `PatchFileTool` — permission request, preview, execute

**Files:**
- Modify: `crates/aivyx-tools/Cargo.toml` (add `diffy = "0.5.1"` dependency)
- Create: `crates/aivyx-tools/src/tools/patch_file.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs` (add `mod patch_file;` and `pub use patch_file::PatchFileTool;`)
- Modify: `crates/aivyx-tools/src/lib.rs` (add `PatchFileTool` to the crate-root `pub use tools::{...}` re-export list — **do this in this same task**, not deferred to wiring, since `agent_builder.rs`'s `use aivyx_tools::{...}` needs it and the last feature's Task 6 had to self-discover this exact gap; it's known now, so it's not optional here)

**Interfaces:**
- Consumes: `crate::diff::unified_diff` (existing, from `write_file`/`edit_file`), `crate::path_resolve::resolve` (existing).
- Produces: `pub struct PatchFileTool;` (a unit struct, no constructor — same shape as `EditFileTool`/`WriteFileTool`/`DeleteFileTool`) implementing `Tool`. Task 2 registers it as `registry.register(Arc::new(PatchFileTool));` — no arguments.

- [ ] **Step 1: Add the `diffy` dependency**

In `crates/aivyx-tools/Cargo.toml`, add one line in the `[dependencies]` block, alphabetically after `async-trait` and before `futures`:

```toml
diffy = "0.5.1"
```

Run: `cargo build -p aivyx-tools`
Expected: builds cleanly (fetches `diffy` and its transitive deps `hashbrown`/`anstyle`-optional-off/`zlib-rs`-optional-off — neither optional dep is pulled in since no extra features are enabled).

- [ ] **Step 2: Write the failing tests**

Create `crates/aivyx-tools/src/tools/patch_file.rs`:

```rust
use std::path::Path;

use aivyx_sandbox::{ActionKind, DiffContent, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::diff::unified_diff;
use crate::path_resolve::resolve;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct PatchFileArgs {
    /// Path to the file to patch, absolute or relative to the working directory. Must already exist.
    path: String,
    /// Unified-diff patch text to apply to the file's current content.
    patch: String,
}

/// Pure parse-and-apply, shared by `permission_request` (a dry run, to
/// build the diff) and `execute` (the real run) — mirrors `edit_file`'s
/// `apply_edit` shape exactly. `diffy` tolerates hunk line numbers that
/// have drifted from the file's actual current content (a common failure
/// mode for a model-generated patch) by searching nearby for matching
/// context rather than requiring an exact position.
///
/// Note: text with no recognizable diff syntax at all parses successfully
/// as an empty (zero-hunk) patch rather than erroring — applying zero
/// hunks is a no-op, so it's caught by the no-op guard below rather than
/// needing a separate "not a valid patch" error path.
fn apply_patch(old: &str, patch_text: &str) -> Result<String, ToolError> {
    let patch = diffy::Patch::from_str(patch_text)
        .map_err(|err| ToolError::InvalidArguments(format!("could not parse patch: {err}")))?;
    let new = diffy::apply(old, &patch)
        .map_err(|err| ToolError::InvalidArguments(format!("could not apply patch: {err}")))?;
    if new == old {
        return Err(ToolError::InvalidArguments(
            "this patch changes nothing — the file already matches the patched result (or the \
             patch text wasn't recognized as a valid diff at all). Re-check what you intended \
             to change"
                .to_string(),
        ));
    }
    Ok(new)
}

pub struct PatchFileTool;

#[async_trait]
impl Tool for PatchFileTool {
    fn name(&self) -> &str {
        "patch_file"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Apply a unified-diff patch to an existing file. Tolerates hunk line \
                numbers that have drifted from the file's current content (searches nearby for \
                matching context) but still fails if the patch's actual context/added/removed \
                lines don't match anywhere. Fails if the file doesn't exist — use write_file to \
                create a new file, or edit_file for a single exact-substring change."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(PatchFileArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: PatchFileArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(cwd, &args.path);

        let old_content = std::fs::read_to_string(&resolved).map_err(|err| {
            ToolError::ExecutionFailed(format!("cannot patch {}: {err}", resolved.display()))
        })?;
        let new_content = apply_patch(&old_content, &args.patch)?;
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
            arguments_preview: json!({ "path": args.path }),
            preview,
            diff,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: PatchFileArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(&ctx.cwd, &args.path);

        let old_content = tokio::fs::read_to_string(&resolved).await.map_err(|err| {
            ToolError::ExecutionFailed(format!("cannot patch {}: {err}", resolved.display()))
        })?;
        let new_content = apply_patch(&old_content, &args.patch)?;
        tokio::fs::write(&resolved, &new_content).await?;

        Ok(ToolOutput::Ok(format!("patched {}", resolved.display())))
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
    async fn execute_applies_a_patch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "fn foo() {}\n").unwrap();
        let patch = "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-fn foo() {}\n+fn foo() -> i32 { 42 }\n";

        let tool = PatchFileTool;
        let output = tool
            .execute(json!({ "path": "a.txt", "patch": patch }), &ctx(dir.path()))
            .await
            .unwrap();

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("patched"), "text: {text}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "fn foo() -> i32 { 42 }\n"
        );
    }

    #[test]
    fn permission_request_is_action_write_with_a_path_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "fn foo() {}\n").unwrap();
        let patch = "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-fn foo() {}\n+fn foo() -> i32 { 42 }\n";

        let tool = PatchFileTool;
        let request = tool
            .permission_request(&json!({ "path": "a.txt", "patch": patch }), dir.path())
            .unwrap();

        assert_eq!(request.action, ActionKind::Write);
        let PermissionTarget::Path(path) = &request.target else {
            panic!("expected a Path target, got {:?}", request.target);
        };
        assert_eq!(path, &dir.path().join("a.txt"));
    }

    #[test]
    fn preview_and_diff_reflect_real_applied_content_not_the_raw_patch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "fn foo() {}\n").unwrap();
        let patch = "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-fn foo() {}\n+fn foo() -> i32 { 42 }\n";

        let tool = PatchFileTool;
        let request = tool
            .permission_request(&json!({ "path": "a.txt", "patch": patch }), dir.path())
            .unwrap();

        let diff = request.diff.expect("expected diff content");
        assert_eq!(diff.old_content, "fn foo() {}\n");
        assert_eq!(diff.new_content, "fn foo() -> i32 { 42 }\n");
        let preview = request.preview.expect("expected a preview");
        // The unified_diff renderer's own output shape (+/- lines), not
        // the raw input patch text echoed back verbatim.
        assert!(preview.contains("-fn foo() {}"));
        assert!(preview.contains("+fn foo() -> i32 { 42 }"));
    }

    #[test]
    fn fuzzy_matching_applies_a_hunk_with_a_stale_line_number() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\nb\nc\nd\ntarget\nf\ng\nh\n").unwrap();

        // The hunk header claims the change is at line 1, but the actual
        // context ("d" / "target" / "f") only appears at line 4 — diffy
        // must search forward from the stated (wrong) position to find it.
        let patch = "--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n d\n-target\n+TARGET\n f\n";

        let tool = PatchFileTool;
        let request = tool
            .permission_request(&json!({ "path": "a.txt", "patch": patch }), dir.path())
            .unwrap();

        let diff = request.diff.expect("expected diff content");
        assert_eq!(diff.new_content, "a\nb\nc\nd\nTARGET\nf\ng\nh\n");
    }

    #[test]
    fn a_malformed_hunk_header_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x\ny\nz\n").unwrap();
        // Attempts diff syntax but the hunk header itself is invalid —
        // this is a genuine ParsePatchError, distinct from plain prose
        // (which parses as an empty patch, see the no-op test below).
        let patch = "--- a/a.txt\n+++ b/a.txt\n@@ garbage not a real header @@\n-x\n+y\n";

        let tool = PatchFileTool;
        let result = tool.permission_request(&json!({ "path": "a.txt", "patch": patch }), dir.path());

        let Err(ToolError::InvalidArguments(message)) = result else {
            panic!("expected InvalidArguments, got {result:?}");
        };
        assert!(message.contains("could not parse patch"), "message: {message}");
    }

    #[test]
    fn a_patch_whose_context_matches_nowhere_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x\ny\nz\n").unwrap();
        // Well-formed hunk syntax, but "nonexistent" appears nowhere in
        // the file — diffy's fuzzy search exhausts the file and fails.
        let patch = "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-nonexistent\n+replacement\n";

        let tool = PatchFileTool;
        let result = tool.permission_request(&json!({ "path": "a.txt", "patch": patch }), dir.path());

        let Err(ToolError::InvalidArguments(message)) = result else {
            panic!("expected InvalidArguments, got {result:?}");
        };
        assert!(message.contains("could not apply patch"), "message: {message}");
    }

    #[test]
    fn a_nonexistent_target_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let patch = "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-x\n+y\n";

        let tool = PatchFileTool;
        let result = tool.permission_request(
            &json!({ "path": "does-not-exist.txt", "patch": patch }),
            dir.path(),
        );

        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }

    #[test]
    fn a_patch_that_changes_nothing_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "same\n").unwrap();
        // Valid patch syntax, but the removed and added lines are
        // identical — applying it produces byte-identical content.
        let patch = "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-same\n+same\n";

        let tool = PatchFileTool;
        let result = tool.permission_request(&json!({ "path": "a.txt", "patch": patch }), dir.path());

        let Err(ToolError::InvalidArguments(message)) = result else {
            panic!("expected InvalidArguments, got {result:?}");
        };
        assert!(message.contains("changes nothing"), "message: {message}");
    }

    #[test]
    fn completely_non_diff_text_is_rejected_as_a_no_op_not_a_parse_error() {
        // Verified behavior: text with no recognizable diff syntax at all
        // parses successfully as an empty (zero-hunk) patch rather than
        // erroring, so applying it is indistinguishable from a no-op —
        // this locks in that the no-op guard is what actually catches
        // this case, not a "malformed patch" branch.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "unchanged content\n").unwrap();
        let patch = "this is not a patch at all, just prose";

        let tool = PatchFileTool;
        let result = tool.permission_request(&json!({ "path": "a.txt", "patch": patch }), dir.path());

        let Err(ToolError::InvalidArguments(message)) = result else {
            panic!("expected InvalidArguments, got {result:?}");
        };
        assert!(message.contains("changes nothing"), "message: {message}");
    }
}
```

Register the new module in `crates/aivyx-tools/src/tools/mod.rs`: add `mod patch_file;` alphabetically (after `mod mcp_tool;`, before `mod move_file;`) and `pub use patch_file::PatchFileTool;` alphabetically in the `pub use` block (after `pub use mcp_tool::McpToolAdapter;`, before `pub use move_file::MoveFileTool;`).

Add `PatchFileTool` to `crates/aivyx-tools/src/lib.rs`'s `pub use tools::{...}` block (currently lines 32-39), alphabetically after `McpToolAdapter` and before `MoveFileTool`:

```rust
pub use tools::{
    DeleteFileTool, EditFileTool, FindReferencesTool, GetMcpPromptTool, GitBranchTool,
    GitCommitTool, GitPrTool, GitPushTool, GitReadTool, GlobTool, GoToDefinitionTool, GrepTool,
    ListMcpPromptsTool, ListMcpResourcesTool, McpToolAdapter, MoveFileTool, PatchFileTool,
    ReadFileTool, ReadMcpResourceTool, RememberPreferenceTool, ReplSendTool, ReplStartTool,
    ReplStopTool, RunCommandTool, RunShellTool, SetTasksTool, SharedReplSession, WebFetchTool,
    WebSearchTool, WriteFileTool, new_shared_repl_session,
};
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p aivyx-tools -- --test-threads=1 patch_file`
Expected: PASS (9 tests: `execute_applies_a_patch`, `permission_request_is_action_write_with_a_path_target`, `preview_and_diff_reflect_real_applied_content_not_the_raw_patch`, `fuzzy_matching_applies_a_hunk_with_a_stale_line_number`, `a_malformed_hunk_header_is_rejected`, `a_patch_whose_context_matches_nowhere_is_rejected`, `a_nonexistent_target_file_is_rejected`, `a_patch_that_changes_nothing_is_rejected`, `completely_non_diff_text_is_rejected_as_a_no_op_not_a_parse_error`).

- [ ] **Step 4: Commit**

```bash
git add crates/aivyx-tools/Cargo.toml crates/aivyx-tools/src/tools/patch_file.rs crates/aivyx-tools/src/tools/mod.rs crates/aivyx-tools/src/lib.rs
git commit -m "Add patch_file tool: apply a unified-diff patch via diffy's fuzzy hunk matching"
```

---

### Task 2: Wire `patch_file` into the agent and document it

**Files:**
- Modify: `crates/aivyx/src/agent_builder.rs` (import at lines 24-31, registration after line 227)
- Modify: `README.md` (Tools table, a short prose paragraph)
- Modify: `ROADMAP.md` (remove the "Patch-apply tool" backlog bullet)

**Interfaces:**
- Consumes: `aivyx_tools::PatchFileTool` (Task 1) — a unit struct, registered with no constructor arguments (unlike `MoveFileTool::new(deny_paths.clone())` on the line immediately above it).

- [ ] **Step 1: Add the import**

In `crates/aivyx/src/agent_builder.rs`, change the `use aivyx_tools::{...}` block (lines 24-31) to add `PatchFileTool` alphabetically, right after `MoveFileTool`:

```rust
use aivyx_tools::{
    CommandSpec, DeleteFileTool, EditFileTool, FindReferencesTool, GetMcpPromptTool,
    GitBranchTool, GitCheckpointer, GitCommitTool, GitPrTool, GitPushTool, GitReadTool, GlobTool,
    GoToDefinitionTool, GrepTool, ListMcpPromptsTool, ListMcpResourcesTool, LspClient, McpClient,
    McpToolAdapter, MoveFileTool, PatchFileTool, ReadFileTool, ReadMcpResourceTool,
    RememberPreferenceTool, ReplSendTool, ReplStartTool, ReplStopTool, RunCommandTool,
    RunShellTool, SetTasksTool, ToolExecutor, ToolRegistry, WebFetchTool, WebSearchTool,
    WriteFileTool, new_shared_repl_session,
};
```

- [ ] **Step 2: Register the tool**

In `crates/aivyx/src/agent_builder.rs`, add a line right after `registry.register(Arc::new(MoveFileTool::new(deny_paths.clone())));` (line 227):

```rust
    registry.register(Arc::new(MoveFileTool::new(deny_paths.clone())));
    registry.register(Arc::new(PatchFileTool));
```

- [ ] **Step 3: Build to confirm it compiles**

Run: `cargo build -p aivyx`
Expected: builds cleanly

- [ ] **Step 4: Update README.md's Tools table**

In `README.md`, add a row right after the `edit_file` row (line 697):

```markdown
| `edit_file` | exact-substring replace in a file | prompt (then cacheable) |
| `patch_file` | apply a unified-diff patch to an existing file | prompt (then cacheable) |
```

- [ ] **Step 5: Add a README prose paragraph**

In `README.md`, add right after the `move_file(from, to)` paragraph (ends at line 502, before "Build/test the workspace:"):

```markdown
**`patch_file(path, patch)`**: applies a unified-diff patch to an
existing file via the `diffy` crate, reusing `ActionKind::Write` — this
is content mutation on an existing path, exactly like `edit_file`, so it
needed no new gate primitive (unlike `delete_file`'s/`move_file`'s own
first-of-their-kind `ActionKind`s). Tolerates hunk line numbers that have
drifted from the file's actual current content — `diffy` searches nearby
for matching context rather than requiring an exact position, since a
model-generated patch's line numbers drift easily even when its actual
content is correct — but still fails clearly if the patch's context
doesn't match anywhere. Existing files only; a patch that would create a
new file or delete one entirely isn't supported — use `write_file`/
`delete_file` for those.
```

- [ ] **Step 6: Remove the closed backlog item from ROADMAP.md**

In `ROADMAP.md`, under "## Backlog — capability opportunities, not yet
scheduled", change:

```markdown
sources, and Plan mode not surviving `--resume`). The remaining two are
sized as their own features — each needs a real design pass (tool-trait
shape, permission/`ActionKind` wiring, config surface) rather than a
same-session patch — so they're tracked here instead of built ad hoc:

- **Patch-apply tool**: edits go through `edit_file` (single search/replace)
  or a full `write_file` rewrite; there's no tool that takes ready-made
  unified-diff/patch text and applies it directly. Relevant when a model
  (or the user) already has a well-formed patch rather than needing to
  re-derive one as a search/replace pair.
- **Verification test-selection**: enforced verification always re-runs
```

to:

```markdown
sources, and Plan mode not surviving `--resume`). The remaining one is
sized as its own feature — it needs a real design pass (tool-trait
shape, permission/`ActionKind` wiring, config surface) rather than a
same-session patch — so it's tracked here instead of built ad hoc:

- **Verification test-selection**: enforced verification always re-runs
```

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx/src/agent_builder.rs README.md ROADMAP.md
git commit -m "Wire patch_file into the agent; document it; close the backlog item"
```

---

### Task 3: Full workspace verification

**Files:** none (verification only)

- [ ] **Step 1: Build the whole workspace**

Run: `cargo build --workspace`
Expected: builds cleanly

- [ ] **Step 2: Run the whole test suite**

Run: `cargo test --workspace -- --test-threads=1`
Expected: PASS — every test in every crate, including all 9 tests added in Task 1

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --workspace --all-targets`
Expected: no warnings

- [ ] **Step 4: Manual smoke test (not automatable — requires a live TUI session)**

Run `cargo run -p aivyx` in a real project directory, have a real model (or a hand-driven call) generate a genuine multi-hunk unified diff for a real change and apply it via `patch_file`, and confirm: the permission modal renders the recomputed diff correctly, approving it produces the correct file content, and the pre-mutation checkpoint fired. This is the live-E2E verification step the design doc calls for — record the result in a follow-up note, but it is not a `- [ ]` step this plan can check off automatically.

If any step fails, stop and fix before proceeding — do not commit on top of a failing workspace state.
