use std::path::{Path, PathBuf};

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget, path_is_denied};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use ignore::WalkBuilder;
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

        let metadata = std::fs::metadata(&from).map_err(|_| {
            ToolError::ExecutionFailed(format!("{} does not exist", from.display()))
        })?;
        if std::fs::metadata(&to).is_ok() {
            return Err(ToolError::ExecutionFailed(format!(
                "{} already exists — move_file refuses to overwrite; delete it first if that's \
                 intended",
                to.display()
            )));
        }

        let preview = if metadata.is_dir() {
            if let Some(denied) = find_denied_descendant(&from, &self.deny_paths) {
                return Err(ToolError::ExecutionFailed(format!(
                    "{} is under a configured deny_paths entry — refusing to move a directory \
                     that contains it (moving would relocate it outside deny_paths' protection)",
                    denied.display()
                )));
            }
            format!(
                "Move directory {} to {}\n\n{}",
                from.display(),
                to.display(),
                directory_listing(&from)
            )
        } else {
            match std::fs::read_to_string(&from) {
                Ok(content) => {
                    format!("Move {} to {}\n\n{}", from.display(), to.display(), content)
                }
                Err(_) => format!(
                    "WARNING: {} could not be read as text (binary file?). This will move it to \
                     {} unchanged.",
                    from.display(),
                    to.display()
                ),
            }
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

/// Independent of `permission_request`'s own `from`/`to` top-level check
/// (`ConfirmationGate::is_denied`, which only sees the two endpoints) — a
/// nested `deny_paths` entry several levels inside a directory being moved
/// would otherwise silently relocate to a path `deny_paths` no longer
/// matches. `standard_filters(false)` is deliberate and load-bearing:
/// unlike `grep`/`glob`'s gitignore-aware walk (relevance, not security),
/// this scan must see every real entry regardless of `.gitignore` — see
/// `a_gitignored_deny_path_nested_in_the_directory_is_still_caught`.
fn find_denied_descendant(root: &Path, deny_paths: &[PathBuf]) -> Option<PathBuf> {
    for entry in WalkBuilder::new(root).standard_filters(false).build() {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path_is_denied(path, deny_paths) {
            return Some(path.to_path_buf());
        }
    }
    None
}

/// Cosmetic only (not security-relevant, unlike `find_denied_descendant`
/// above) — gitignore-aware and capped, matching `glob.rs`'s own
/// `MAX_PATHS` truncation convention, so a human isn't shown an unbounded
/// dump for a large directory.
const MAX_LISTED_ENTRIES: usize = 200;

fn directory_listing(root: &Path) -> String {
    let mut entries: Vec<String> = Vec::new();
    let mut truncated = false;
    for entry in WalkBuilder::new(root).build() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        if entries.len() >= MAX_LISTED_ENTRIES {
            truncated = true;
            break;
        }
        let path = entry.path();
        entries.push(path.strip_prefix(root).unwrap_or(path).display().to_string());
    }
    let mut output = entries.join("\n");
    if truncated {
        output.push_str(&format!(
            "\n... {MAX_LISTED_ENTRIES}+ files, showing first {MAX_LISTED_ENTRIES}"
        ));
    }
    if output.is_empty() {
        output = "(empty directory)".to_string();
    }
    output
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
    fn moving_a_path_onto_itself_is_rejected_as_an_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let result = tool.permission_request(&json!({ "from": "a.txt", "to": "a.txt" }), dir.path());

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

    #[tokio::test]
    async fn execute_moves_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() {}\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let output = tool
            .execute(json!({ "from": "src", "to": "lib" }), &ctx(dir.path()))
            .await
            .unwrap();

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("moved"), "text: {text}");
        assert!(!dir.path().join("src").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("lib/a.rs")).unwrap(),
            "fn a() {}\n"
        );
    }

    #[test]
    fn an_existing_destination_directory_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::create_dir(dir.path().join("lib")).unwrap();

        let tool = MoveFileTool::new(vec![]);
        let result = tool.permission_request(&json!({ "from": "src", "to": "lib" }), dir.path());

        let Err(ToolError::ExecutionFailed(message)) = result else {
            panic!("expected ExecutionFailed, got {result:?}");
        };
        assert!(message.contains("already exists"), "message: {message}");
    }

    #[test]
    fn preview_lists_directory_contents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.path().join("src/b.rs"), "fn b() {}\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let request = tool
            .permission_request(&json!({ "from": "src", "to": "lib" }), dir.path())
            .unwrap();

        let preview = request.preview.expect("expected a preview");
        assert!(preview.contains("a.rs"));
        assert!(preview.contains("b.rs"));
    }

    #[test]
    fn preview_truncates_directory_listing_when_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        for i in 0..(MAX_LISTED_ENTRIES + 5) {
            std::fs::write(dir.path().join(format!("src/f{i}.txt")), "").unwrap();
        }

        let tool = MoveFileTool::new(vec![]);
        let request = tool
            .permission_request(&json!({ "from": "src", "to": "lib" }), dir.path())
            .unwrap();

        let preview = request.preview.expect("expected a preview");
        let listed = preview.lines().filter(|l| l.ends_with(".txt")).count();
        assert_eq!(listed, MAX_LISTED_ENTRIES);
        assert!(preview.contains(&format!("{MAX_LISTED_ENTRIES}+ files")));
    }

    #[test]
    fn a_directory_move_is_refused_when_a_deny_path_is_nested_inside_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("secrets")).unwrap();
        std::fs::write(dir.path().join("secrets/.env"), "SECRET=1\n").unwrap();

        let tool = MoveFileTool::new(vec![dir.path().join("secrets/.env")]);
        let result = tool.permission_request(
            &json!({ "from": "secrets", "to": "archive/secrets" }),
            dir.path(),
        );

        let Err(ToolError::ExecutionFailed(message)) = result else {
            panic!("expected ExecutionFailed, got {result:?}");
        };
        assert!(message.contains(".env"), "message: {message}");
        // Nothing was moved — the walk happens before any mutation.
        assert!(dir.path().join("secrets/.env").exists());
    }

    #[test]
    fn a_gitignored_deny_path_nested_in_the_directory_is_still_caught() {
        // Regression test for the reason this scan can't reuse grep/glob's
        // own walk unmodified: a `.env` is exactly the kind of file that's
        // both deny_paths-worthy and routinely gitignored. If the scan were
        // gitignore-aware, a nested .env would be silently invisible to it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "secrets/.env\n").unwrap();
        std::fs::create_dir(dir.path().join("secrets")).unwrap();
        std::fs::write(dir.path().join("secrets/.env"), "SECRET=1\n").unwrap();

        let tool = MoveFileTool::new(vec![dir.path().join("secrets/.env")]);
        let result = tool.permission_request(
            &json!({ "from": "secrets", "to": "archive/secrets" }),
            dir.path(),
        );

        assert!(
            matches!(result, Err(ToolError::ExecutionFailed(_))),
            "a gitignored deny_paths entry must still block the move"
        );
    }

    #[test]
    fn a_non_dotfile_deny_path_hidden_only_by_gitignore_is_still_caught() {
        // Regression test distinguishing the git-ignore-specific mechanism
        // from the hidden-dotfile filter the sibling test above actually
        // exercises: `ignore::WalkBuilder` only consults `.gitignore` when a
        // `.git` directory is present (`require_git` defaults to true), so
        // this test creates a real one. `secrets/creds.txt` is not a
        // dotfile, so the hidden-file filter can't be what's hiding it here
        // — only gitignore awareness could, and `standard_filters(false)`
        // must defeat it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "secrets/creds.txt\n").unwrap();
        std::fs::create_dir(dir.path().join("secrets")).unwrap();
        std::fs::write(dir.path().join("secrets/creds.txt"), "SECRET=1\n").unwrap();

        let tool = MoveFileTool::new(vec![dir.path().join("secrets/creds.txt")]);
        let result = tool.permission_request(
            &json!({ "from": "secrets", "to": "archive/secrets" }),
            dir.path(),
        );

        assert!(
            matches!(result, Err(ToolError::ExecutionFailed(_))),
            "a deny_paths entry hidden only by .gitignore (not a dotfile) must still block the move"
        );
    }
}
