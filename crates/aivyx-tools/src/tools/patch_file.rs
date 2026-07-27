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
