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
#[allow(dead_code)]
pub(crate) const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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
#[allow(dead_code)]
pub(crate) struct Cursor {
    pub(crate) line: u32,
    pub(crate) column: u32,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
