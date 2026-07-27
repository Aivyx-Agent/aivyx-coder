use std::path::{Path, PathBuf};

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::path_resolve::resolve;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct MoveFileArgs {
    /// Path to the file or directory to move, absolute or relative to the working directory.
    from: String,
    /// Destination path, absolute or relative to the working directory. Must not already exist.
    to: String,
}

/// `ActionKind::Move`'s only constructor. Atomic rename via a single
/// `tokio::fs::rename` — works for files and whole directory trees on the
/// same filesystem in one syscall. Refuses if `to` already exists (no
/// overwrite mode) or if `from`/`to` end up on different filesystems
/// (`EXDEV`/`CrossesDevices` is surfaced directly, no copy+delete
/// fallback) — see the design doc's resolved questions for why both are
/// refusals, not silent alternate behavior.
pub struct MoveFileTool {
    /// Only consulted for a directory `from` (see `find_denied_descendant`
    /// in the directory-support pass) — the top-level
    /// `ConfirmationGate::is_denied` check already covers a `from`/`to`
    /// that directly matches a `deny_paths` entry; this field exists so a
    /// directory move can also refuse when a denied path lives *nested
    /// inside* the tree being relocated, the same reasoning
    /// `GrepTool`/`GlobTool` already document for needing `deny_paths`
    /// beyond `permission_request`'s single-target check.
    deny_paths: Vec<PathBuf>,
}

impl MoveFileTool {
    pub fn new(deny_paths: Vec<PathBuf>) -> Self {
        Self { deny_paths }
    }
}

#[async_trait]
impl Tool for MoveFileTool {
    fn name(&self) -> &str {
        "move_file"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Move or rename a file or directory. Refuses if the destination \
                already exists (no overwrite) or if source and destination are on different \
                filesystems (no copy+delete fallback). Atomic on the same filesystem."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(MoveFileArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: MoveFileArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let from = resolve(cwd, &args.from);
        let to = resolve(cwd, &args.to);

        std::fs::metadata(&from).map_err(|_| {
            ToolError::ExecutionFailed(format!("{} does not exist", from.display()))
        })?;
        if std::fs::metadata(&to).is_ok() {
            return Err(ToolError::ExecutionFailed(format!(
                "{} already exists — move_file refuses to overwrite; delete it first if that's \
                 intended",
                to.display()
            )));
        }

        let preview = match std::fs::read_to_string(&from) {
            Ok(content) => format!("Move {} to {}\n\n{}", from.display(), to.display(), content),
            Err(_) => format!(
                "WARNING: {} could not be read as text (binary file?). This will move it to {} \
                 unchanged.",
                from.display(),
                to.display()
            ),
        };

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Move,
            target: PermissionTarget::Move { from, to },
            arguments_preview: json!({ "from": args.from, "to": args.to }),
            preview: Some(preview),
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: MoveFileArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let from = resolve(&ctx.cwd, &args.from);
        let to = resolve(&ctx.cwd, &args.to);

        // Re-checked here, not just in permission_request: narrows (does not
        // eliminate) the window between the user approving the move and this
        // call actually running, during which something could have created
        // `to` — rename(2) would otherwise silently replace it. symlink_metadata
        // (not metadata) so a symlink planted at `to` is caught without
        // following it.
        if std::fs::symlink_metadata(&to).is_ok() {
            return Err(ToolError::ExecutionFailed(format!(
                "{} now exists (created after this move was approved) — refusing to overwrite it",
                to.display()
            )));
        }

        tokio::fs::rename(&from, &to)
            .await
            .map_err(|err| map_rename_error(err, &from, &to))?;

        Ok(ToolOutput::Ok(format!("moved {} to {}", from.display(), to.display())))
    }
}

/// Split out of `execute` so the `EXDEV` mapping is unit-testable without
/// two real filesystems in CI — construct the `io::Error` directly instead.
fn map_rename_error(err: std::io::Error, from: &Path, to: &Path) -> ToolError {
    if err.kind() == std::io::ErrorKind::CrossesDevices {
        ToolError::ExecutionFailed(format!(
            "{} and {} are on different filesystems — move_file only performs an atomic \
             same-filesystem rename and does not fall back to copy+delete",
            from.display(),
            to.display()
        ))
    } else {
        ToolError::Io(err)
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
    async fn execute_moves_a_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "hello\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let output = tool
            .execute(json!({ "from": "old.txt", "to": "new.txt" }), &ctx(dir.path()))
            .await
            .unwrap();

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("moved"), "text: {text}");
        assert!(!dir.path().join("old.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "hello\n"
        );
    }

    #[test]
    fn permission_request_is_action_move_with_a_move_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "hello\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let request = tool
            .permission_request(&json!({ "from": "old.txt", "to": "new.txt" }), dir.path())
            .unwrap();

        assert_eq!(request.action, ActionKind::Move);
        let PermissionTarget::Move { from, to } = &request.target else {
            panic!("expected a Move target, got {:?}", request.target);
        };
        assert_eq!(from, &dir.path().join("old.txt"));
        assert_eq!(to, &dir.path().join("new.txt"));
    }

    #[test]
    fn preview_shows_source_content_for_a_text_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "important notes\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let request = tool
            .permission_request(&json!({ "from": "old.txt", "to": "new.txt" }), dir.path())
            .unwrap();

        let preview = request.preview.expect("expected a preview");
        assert!(preview.contains("important notes"));
        assert!(preview.contains("old.txt"));
        assert!(preview.contains("new.txt"));
    }

    #[test]
    fn preview_warns_instead_of_showing_content_for_a_binary_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.bin"), [0xFF, 0xFE, 0x00, 0xD8, 0x00]).unwrap();

        let tool = MoveFileTool::new(vec![]);
        let request = tool
            .permission_request(&json!({ "from": "data.bin", "to": "moved.bin" }), dir.path())
            .unwrap();

        let preview = request.preview.expect("expected a warning preview, not None");
        assert!(preview.contains("WARNING"));
        assert!(preview.contains("binary"));
    }

    #[test]
    fn a_nonexistent_source_is_rejected() {
        let dir = tempfile::tempdir().unwrap();

        let tool = MoveFileTool::new(vec![]);
        let result = tool.permission_request(
            &json!({ "from": "does-not-exist.txt", "to": "new.txt" }),
            dir.path(),
        );

        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }

    #[test]
    fn an_existing_destination_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "hello\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "already here\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let result =
            tool.permission_request(&json!({ "from": "old.txt", "to": "new.txt" }), dir.path());

        let Err(ToolError::ExecutionFailed(message)) = result else {
            panic!("expected ExecutionFailed, got {result:?}");
        };
        assert!(message.contains("already exists"), "message: {message}");
    }

    #[test]
    fn cross_filesystem_rename_error_is_surfaced_clearly() {
        let err = std::io::Error::from(std::io::ErrorKind::CrossesDevices);
        let mapped = map_rename_error(err, Path::new("/a/old.txt"), Path::new("/b/new.txt"));
        let ToolError::ExecutionFailed(message) = mapped else {
            panic!("expected ExecutionFailed, got {mapped:?}");
        };
        assert!(message.contains("different filesystems"), "message: {message}");
    }

    #[test]
    fn other_io_errors_pass_through_unmapped() {
        let err = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let mapped = map_rename_error(err, Path::new("/a/old.txt"), Path::new("/b/new.txt"));
        assert!(matches!(mapped, ToolError::Io(_)));
    }

    #[tokio::test]
    async fn execute_refuses_a_destination_that_appeared_after_permission_was_granted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "hello\n").unwrap();
        // Simulates the race: permission_request ran (and approved) when
        // new.txt didn't exist yet, but something created it before execute()
        // ran — calling execute() directly here skips permission_request
        // entirely, which is exactly that scenario.
        std::fs::write(dir.path().join("new.txt"), "raced in\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let result = tool
            .execute(json!({ "from": "old.txt", "to": "new.txt" }), &ctx(dir.path()))
            .await;

        assert!(result.is_err(), "execute must refuse rather than silently clobber");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "raced in\n",
            "the pre-existing destination content must survive"
        );
        assert!(dir.path().join("old.txt").exists(), "the source must not have moved");
    }
}
