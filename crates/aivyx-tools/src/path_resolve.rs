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
///
/// `pub` (re-exported as `aivyx_tools::resolve`), not `pub(crate)`, so a
/// consumer outside this crate — `aivyx`'s `agent_builder.rs`, building
/// `MoldConfig::output_dir` — can canonicalize `assets/generated` through
/// the exact same helper `GenerateImageTool`/`GenerateThreeDTool` use for
/// their own `PermissionTarget`, rather than a second, independently
/// written `cwd.join(...)` that could silently drift from what the
/// permission system actually reasons about.
pub fn resolve(cwd: &Path, raw: &str) -> PathBuf {
    let lexical = resolve_lexical(cwd, raw);
    resolve_symlinks(&lexical)
}

/// The reason to refuse `raw` when it is the project's own absolute path
/// without its leading `/` (`k/runs/app/src/x.rs` for a project at
/// `/k/runs/app`): resolved against the project, it would make a copy of
/// that folder chain inside it, as a model running in a container did
/// (fltplan build test, 2026-10-22). `None` otherwise — for a project path
/// of one component (too short to tell a slip from a real folder), and when
/// a folder of that name really exists.
pub fn missing_leading_slash(cwd: &Path, raw: &str) -> Option<String> {
    if raw.starts_with('/') || raw.starts_with('~') {
        return None;
    }
    let project: Vec<Component> =
        cwd.components().filter(|c| matches!(c, Component::Normal(_))).collect();
    let given: Vec<Component> =
        Path::new(raw).components().filter(|c| !matches!(c, Component::CurDir)).collect();
    if project.len() < 2
        || given.len() < project.len()
        || given[..project.len()] != project[..]
        || cwd.join(given[0]).exists()
    {
        return None;
    }
    let rest: PathBuf = given[project.len()..].iter().collect();
    let rest = if rest.as_os_str().is_empty() {
        ".".to_string()
    } else {
        rest.display().to_string()
    };
    Some(format!(
        "`{raw}` looks like this project's own path `{}` without its leading `/`. Paths are \
         relative to the project: use `{rest}`.",
        cwd.display()
    ))
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
    // Path components still to re-append, innermost last.
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    // Bounds dangling-link chains and loops (Linux's own limit is 40).
    let mut hops = 0;

    loop {
        if let Ok(canonical) = current.canonicalize() {
            let mut result = canonical;
            for component in tail.into_iter().rev() {
                result.push(component);
            }
            return result;
        }

        // A symlink whose target doesn't exist (yet): canonicalize fails,
        // but a write through it creates the target — so resolve to where
        // it points, or a link inside the project would pass the gate's
        // checks while the write lands outside (or in a denied path).
        if hops < 40
            && std::fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_symlink())
            && let Ok(target) = std::fs::read_link(&current)
        {
            hops += 1;
            let base = current.parent().map(Path::to_path_buf).unwrap_or_default();
            current = normalize(&if target.is_absolute() { target } else { base.join(target) });
            continue;
        }

        match (current.file_name().map(ToOwned::to_owned), current.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                current = parent.to_path_buf();
            }
            // No ancestor could be canonicalized (shouldn't happen on a
            // real filesystem — `/` always exists). Fail safe: return the
            // lexical path unchanged rather than panicking.
            _ => return path.to_path_buf(),
        }
    }
}

/// `.`/`..` removed lexically, for a symlink target joined onto its
/// link's folder.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !matches!(out.components().next_back(), Some(Component::RootDir) | None) {
                    out.pop();
                }
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_path_missing_its_leading_slash_is_caught_with_the_right_path() {
        let cwd = Path::new("/k/runs/9b/app");
        let msg = missing_leading_slash(cwd, "k/runs/9b/app/src/main.rs").unwrap();
        assert!(msg.contains("`/k/runs/9b/app`") && msg.contains("`src/main.rs`"), "{msg}");
        let msg = missing_leading_slash(cwd, "./k/runs/9b/app").unwrap();
        assert!(msg.contains("`.`"), "{msg}");
    }

    #[test]
    fn ordinary_and_absolute_paths_pass() {
        let cwd = Path::new("/k/runs/9b/app");
        for raw in ["src/main.rs", "/k/runs/9b/app/src/main.rs", "~/notes.md", "k/other.rs", "k"] {
            assert_eq!(missing_leading_slash(cwd, raw), None, "{raw}");
        }
        // One component is too short to tell a slip from a real folder.
        assert_eq!(missing_leading_slash(Path::new("/app"), "app/x.rs"), None);
    }

    #[test]
    fn a_real_folder_with_that_name_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let rel = cwd.strip_prefix("/").unwrap().to_path_buf();
        let raw = rel.join("x.rs").to_string_lossy().into_owned();
        assert!(missing_leading_slash(&cwd, &raw).is_some(), "{raw}");
        let first = rel.components().next().unwrap();
        std::fs::create_dir(cwd.join(first)).unwrap();
        assert_eq!(missing_leading_slash(&cwd, &raw), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_resolves_to_where_it_points() {
        // A link inside the project to a file that doesn't exist yet: a
        // write through it lands at the target, so that's what the gate
        // must judge — not the in-project link path.
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().canonicalize().unwrap().join("planted.txt");
        std::os::unix::fs::symlink(&target, project.path().join("notes.md")).unwrap();
        assert_eq!(resolve(project.path(), "notes.md"), target);

        // Relative targets resolve against the link's folder, through chains.
        std::fs::create_dir(project.path().join("docs")).unwrap();
        std::os::unix::fs::symlink("../notes.md", project.path().join("docs/link.md")).unwrap();
        assert_eq!(resolve(project.path(), "docs/link.md"), target);

        // A path *under* a dangling directory link resolves through it too.
        std::os::unix::fs::symlink(outside.path().join("newdir"), project.path().join("d")).unwrap();
        assert_eq!(
            resolve(project.path(), "d/x.txt"),
            outside.path().canonicalize().unwrap().join("newdir/x.txt")
        );

        // A loop doesn't hang.
        std::os::unix::fs::symlink("loop-b", project.path().join("loop-a")).unwrap();
        std::os::unix::fs::symlink("loop-a", project.path().join("loop-b")).unwrap();
        let _ = resolve(project.path(), "loop-a");
    }

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
