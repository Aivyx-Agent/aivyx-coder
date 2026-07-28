use std::path::{Component, Path, PathBuf};

/// Resolves a (possibly relative, possibly `~`-prefixed) tool-supplied path
/// against `cwd` into an absolute, symlink-resolved path.
///
/// Symlink resolution matters for more than correctness: `ConfirmationGate`
/// matches `deny_paths` against whatever this function returns. If a
/// symlink under `cwd` pointed at a denied directory (e.g. `~/.ssh`) were
/// left unresolved, the permission check would see only the
/// harmless-looking symlink path while the actual file I/O — which *does*
/// follow symlinks — reaches the real, denied target, silently defeating
/// the deny-list.
///
/// Resolution happens in two passes: `resolve_lexical` handles `~`
/// expansion and `.`/`..` normalization without touching the filesystem
/// (needed because a `write_file` target may not exist yet), then
/// `resolve_symlinks` canonicalizes as much of the result as actually
/// exists on disk. Reading filesystem metadata (stat/readlink, no writes)
/// is safe to do from `Tool::permission_request` — its contract forbids
/// mutating side effects, not filesystem reads.
pub(crate) fn resolve(cwd: &Path, raw: &str) -> PathBuf {
    let lexical = resolve_lexical(cwd, raw);
    resolve_symlinks(&lexical)
}

fn resolve_lexical(cwd: &Path, raw: &str) -> PathBuf {
    let expanded: PathBuf = if let Some(rest) = raw.strip_prefix("~/") {
        home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(raw))
    } else if raw == "~" {
        home_dir().unwrap_or_else(|| PathBuf::from(raw))
    } else {
        PathBuf::from(raw)
    };

    let joined = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(&expanded)
    };

    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                if !matches!(
                    normalized.components().next_back(),
                    Some(Component::RootDir) | None
                ) {
                    normalized.pop();
                } else if normalized.components().next_back().is_none() {
                    normalized.push(component);
                }
            }
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    normalized
}

/// Canonicalizes as much of `path` as exists, then re-appends whatever
/// doesn't — so a not-yet-created `write_file` target still gets
/// symlink-safe treatment via its (existing) parent directories.
fn resolve_symlinks(path: &Path) -> PathBuf {
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut current = path;

    loop {
        if let Ok(canonical) = current.canonicalize() {
            let mut result = canonical;
            for component in tail.into_iter().rev() {
                result.push(component);
            }
            return result;
        }

        match (current.file_name(), current.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                current = parent;
            }
            // No ancestor could be canonicalized (shouldn't happen on a
            // real filesystem — `/` always exists). Fail safe: return the
            // lexical path unchanged rather than panicking.
            _ => return path.to_path_buf(),
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_joins_onto_cwd() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();

        let resolved = resolve(dir.path(), "src/main.rs");

        let expected = dir.path().canonicalize().unwrap().join("src/main.rs");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn absolute_path_passes_through() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("hosts");

        let resolved = resolve(Path::new("/irrelevant"), target.to_str().unwrap());

        let expected = dir.path().canonicalize().unwrap().join("hosts");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn dot_dot_components_are_normalized_lexically() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();

        let resolved = resolve(&project, "../other/file.rs");

        let expected = dir.path().canonicalize().unwrap().join("other/file.rs");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn dot_components_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();

        let resolved = resolve(dir.path(), "./src/./main.rs");

        let expected = dir.path().canonicalize().unwrap().join("src/main.rs");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn leading_dot_dot_beyond_root_stays_relative_rather_than_panicking() {
        let resolved = resolve(Path::new("/"), "../../etc/passwd");

        // Can't go above root; lexical normalization just stops consuming.
        // `/etc/passwd` itself is then canonicalized like anything else.
        let expected = PathBuf::from("/etc/passwd")
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from("/etc/passwd"));
        assert_eq!(resolved, expected);
    }

    #[test]
    fn tilde_prefixed_path_resolves_against_the_real_home_directory() {
        let home = std::env::var("HOME").expect("HOME must be set for this test");
        let canonical_home = PathBuf::from(&home)
            .canonicalize()
            .expect("$HOME must exist");

        let resolved = resolve(
            Path::new("/irrelevant"),
            "~/.aivyx-path-resolve-test-marker",
        );

        assert_eq!(
            resolved,
            canonical_home.join(".aivyx-path-resolve-test-marker")
        );
    }

    #[test]
    fn symlink_is_resolved_to_its_real_target() {
        let dir = tempfile::tempdir().unwrap();
        let real_target = dir.path().join("real_target");
        std::fs::create_dir(&real_target).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real_target, &link).unwrap();

        let resolved = resolve(dir.path(), "link/config.txt");

        let expected = real_target.canonicalize().unwrap().join("config.txt");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn symlink_escaping_cwd_does_not_resolve_to_a_path_under_cwd() {
        // Regression test for the security bug this fix closes: a symlink
        // under cwd pointing elsewhere (standing in for e.g. a symlink to
        // `~/.ssh`) must resolve to its REAL location, not the
        // in-cwd-looking lexical path — otherwise ConfirmationGate's
        // deny_paths check (a `starts_with` on the resolved path) never
        // sees where the file actually lives.
        let outside = tempfile::tempdir().unwrap();
        let cwd_dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), cwd_dir.path().join("innocuous")).unwrap();

        let resolved = resolve(cwd_dir.path(), "innocuous/secret");

        let expected = outside.path().canonicalize().unwrap().join("secret");
        assert_eq!(resolved, expected);
        assert!(!resolved.starts_with(cwd_dir.path().canonicalize().unwrap()));
    }
}
