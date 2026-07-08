use std::path::{Component, Path, PathBuf};

/// Resolves a (possibly relative, possibly `~`-prefixed) tool-supplied path
/// against `cwd` into an absolute path, lexically normalizing `.`/`..`
/// components.
///
/// Deliberately does NOT touch the filesystem (no `canonicalize`) — a
/// `write_file` target may not exist yet, and `Tool::permission_request`
/// must stay free of mutating side effects, so resolution can't depend on
/// what's actually on disk. Known limitation: no symlink resolution.
///
/// `~` expansion matters here for more than convenience: `ConfirmationGate`
/// matches `deny_paths` (e.g. `~/.ssh`, already expanded to an absolute
/// path) against whatever this function returns — if a model-supplied
/// `~/.ssh/config` were left unexpanded, it would resolve to a harmless
/// literal `~` subdirectory under `cwd` instead of the real home directory,
/// silently defeating the deny-list.
pub(crate) fn resolve(cwd: &Path, raw: &str) -> PathBuf {
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

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_prefixed_path_expands_to_the_real_home_directory() {
        let home = std::env::var("HOME").expect("HOME must be set for this test");
        let resolved = resolve(Path::new("/some/cwd"), "~/.ssh/config");
        assert_eq!(resolved, PathBuf::from(home).join(".ssh/config"));
    }

    #[test]
    fn relative_path_joins_onto_cwd() {
        let resolved = resolve(Path::new("/home/user/project"), "src/main.rs");
        assert_eq!(resolved, PathBuf::from("/home/user/project/src/main.rs"));
    }

    #[test]
    fn absolute_path_passes_through() {
        let resolved = resolve(Path::new("/home/user/project"), "/etc/hosts");
        assert_eq!(resolved, PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn dot_dot_components_are_normalized_lexically() {
        let resolved = resolve(Path::new("/home/user/project"), "../other/file.rs");
        assert_eq!(resolved, PathBuf::from("/home/user/other/file.rs"));
    }

    #[test]
    fn dot_components_are_dropped() {
        let resolved = resolve(Path::new("/home/user/project"), "./src/./main.rs");
        assert_eq!(resolved, PathBuf::from("/home/user/project/src/main.rs"));
    }

    #[test]
    fn leading_dot_dot_beyond_root_stays_relative_rather_than_panicking() {
        let resolved = resolve(Path::new("/"), "../../etc/passwd");
        // Can't go above root; lexical normalization just stops consuming.
        assert_eq!(resolved, PathBuf::from("/etc/passwd"));
    }
}
