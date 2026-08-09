//! Topic-prefix validation and cwd-scoped key rewriting shared by
//! `memory_write`/`memory_read`/`memory_forget`.
//!
//! Kept out of `aivyx-core` (which computes an identical cwd hash for
//! session keys in `session.rs`) because `aivyx-tools` does not depend on
//! `aivyx-core` — the dependency direction in this workspace runs the
//! other way. This ~10-line FNV-1a is a deliberate, justified duplicate
//! of `session.rs`'s own, for the same reason `deny_paths` matching was
//! once duplicated between `aivyx-sandbox` and `aivyx-repomap`: the crate
//! boundary matters more than avoiding a small, stable, well-tested
//! duplicate. Must stay byte-identical to `session.rs::fnv1a` — if either
//! changes, update both.

use std::path::Path;

use crate::ToolError;

const GLOBAL_PREFIX: &str = "global:";
const PROJECT_PREFIX: &str = "project:";

/// Rewrites a model-supplied topic into its internal storage key.
/// `project:<rest>` becomes `project:<cwd-hash>:<rest>` so the same
/// nominal topic in two different project directories never collides;
/// `global:<rest>` passes through unchanged. Any other prefix (or no
/// prefix) is rejected so the model can't silently write into an
/// unnamespaced key.
pub(crate) fn resolve_topic(cwd: &Path, raw_topic: &str) -> Result<String, ToolError> {
    if let Some(rest) = raw_topic.strip_prefix(GLOBAL_PREFIX) {
        if rest.is_empty() {
            return Err(ToolError::InvalidArguments(
                "topic must have content after \"global:\"".to_string(),
            ));
        }
        return Ok(raw_topic.to_string());
    }
    if let Some(rest) = raw_topic.strip_prefix(PROJECT_PREFIX) {
        if rest.is_empty() {
            return Err(ToolError::InvalidArguments(
                "topic must have content after \"project:\"".to_string(),
            ));
        }
        let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        return Ok(format!(
            "project:{:016x}:{rest}",
            fnv1a(canonical.to_string_lossy().as_bytes())
        ));
    }
    Err(ToolError::InvalidArguments(format!(
        "topic must start with \"global:\" or \"project:\" (got {raw_topic:?})"
    )))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_prefix_passes_through_unchanged() {
        let resolved = resolve_topic(Path::new("/irrelevant"), "global:editor").unwrap();
        assert_eq!(resolved, "global:editor");
    }

    #[test]
    fn project_prefix_gets_a_cwd_hash_inserted() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_topic(dir.path(), "project:flaky-tests").unwrap();
        assert!(resolved.starts_with("project:"));
        assert!(resolved.ends_with(":flaky-tests"));
        assert_ne!(resolved, "project:flaky-tests");
    }

    #[test]
    fn same_project_topic_resolves_identically_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let first = resolve_topic(dir.path(), "project:flaky-tests").unwrap();
        let second = resolve_topic(dir.path(), "project:flaky-tests").unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn different_project_dirs_resolve_the_same_topic_differently() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let a = resolve_topic(dir_a.path(), "project:flaky-tests").unwrap();
        let b = resolve_topic(dir_b.path(), "project:flaky-tests").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn bare_topic_is_rejected() {
        let err = resolve_topic(Path::new("/irrelevant"), "flaky-tests").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn empty_prefix_content_is_rejected() {
        let err = resolve_topic(Path::new("/irrelevant"), "global:").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
