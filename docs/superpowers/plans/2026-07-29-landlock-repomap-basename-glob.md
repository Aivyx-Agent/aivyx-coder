# Landlock + `aivyx-repomap` Basename-Glob Enforcement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend basename-glob `deny_paths` enforcement (currently only
covering the model's own file/search/git tools) to Landlock's
command-tool grants and `aivyx-repomap`'s own matcher — closing the last
open item from the entire 2026-07-28 capability-audit backlog lineage.

**Architecture:** A new `find_basename_glob_matches` function in
`aivyx-sandbox/src/confiner.rs` resolves bare `deny_paths` patterns into
concrete absolute paths, once per session at `LandlockConfiner`
construction, scoped only to project-relevant grant roots (`cwd` and
`extra_read_paths`) — the existing, unchanged `grant_paths_excluding`
then carves those out exactly like any other denial. A new shared
`is_bare_pattern` predicate keeps this classification consistent with
`path_is_denied`'s own. `aivyx-repomap` gains its own basename-glob
matching (mirroring the canonical logic, since it can't depend on
`aivyx-sandbox`) via a new `globset` dependency — not a constraint
violation, since that crate's real boundary is zero dependency on other
*workspace* crates, not zero external dependencies.

**Tech Stack:** Rust, `globset` (newly added to `aivyx-repomap`; already
present in `aivyx-sandbox` since the `deny_paths` basename-glob feature).

## Global Constraints

- `grant_paths_excluding` (`crates/aivyx-sandbox/src/confiner.rs`) must
  not change at all — every new path resolves bare patterns into
  concrete paths *before* calling it, so it always receives exactly the
  kind of input it already handles.
- The basename-glob-aware scan applies only to `cwd` (both its read and
  write grant) and each `extra_read_paths` entry (read only) — never to
  `DEFAULT_READ_PATHS`, `DEFAULT_HOME_READ_PATHS`, the OS temp directory,
  or `TMPDIR`. This is a deliberate scope limit, not an oversight to
  "complete later."
- `path_is_denied`'s public signature and existing behavior for every
  path-separator-containing entry must not change.
- `aivyx-repomap` must not gain a dependency on `aivyx-sandbox` or any
  other workspace crate — only the new external `globset` dependency.
- `is_bare_pattern`'s classification logic
  (`path.parent() == Some(Path::new(""))`) must be identical wherever it
  appears (`aivyx-sandbox`'s shared function, and `aivyx-repomap`'s own
  local copy, which can't share the function across the crate boundary).

---

### Task 1: Extract `is_bare_pattern` in `aivyx-sandbox`

**Files:**
- Modify: `crates/aivyx-sandbox/src/lib.rs`

**Interfaces:**
- Produces: `fn is_bare_pattern(path: &Path) -> bool` (private to the
  crate root module, so visible to the `confiner` submodule) — Task 2
  depends on calling this from `confiner.rs`.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` block in `crates/aivyx-sandbox/src/lib.rs`
(after the existing `a_path_separator_entry_keeps_exact_prefix_matching`
test):

```rust
    #[test]
    fn is_bare_pattern_is_true_for_a_single_component_entry() {
        assert!(is_bare_pattern(Path::new(".env")));
        assert!(is_bare_pattern(Path::new("*.pem")));
    }

    #[test]
    fn is_bare_pattern_is_false_for_a_path_separator_entry() {
        assert!(!is_bare_pattern(Path::new("/home/user/.ssh")));
        assert!(!is_bare_pattern(Path::new("relative/two/parts")));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-sandbox --test-threads=1`
Expected: compile error — `is_bare_pattern` does not exist yet.

- [ ] **Step 3: Extract the function and use it in `path_is_denied`**

In `crates/aivyx-sandbox/src/lib.rs`, replace:

```rust
pub fn path_is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool {
    deny_paths.iter().any(|denied| {
        if denied.parent() == Some(Path::new("")) {
            is_basename_glob_match(path, denied)
        } else {
            path.starts_with(denied)
        }
    })
}
```

with:

```rust
pub fn path_is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool {
    deny_paths.iter().any(|denied| {
        if is_bare_pattern(denied) {
            is_basename_glob_match(path, denied)
        } else {
            path.starts_with(denied)
        }
    })
}

/// A `deny_paths` entry with a single path component (e.g. `.env`,
/// `*.pem`) is a basename-glob pattern, not a real filesystem location
/// to resolve — see `path_is_denied`'s own doc comment above for the
/// full rationale. Extracted so `confiner.rs`'s
/// `find_basename_glob_matches` classifies entries identically rather
/// than re-deriving the same check independently.
fn is_bare_pattern(path: &Path) -> bool {
    path.parent() == Some(Path::new(""))
}
```

(Leave `path_is_denied`'s existing doc comment above the function
completely unchanged — it still accurately describes the overall
behavior.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-sandbox --test-threads=1`
Expected: all tests pass, including the two new ones and the four
pre-existing `path_is_denied` tests (unaffected — same behavior, just
refactored).

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-sandbox/src/lib.rs
git commit -m "Extract is_bare_pattern, shared by path_is_denied and the upcoming Landlock fix"
```

---

### Task 2: Basename-glob-aware Landlock grants for `cwd`/`extra_read_paths`

**Files:**
- Modify: `crates/aivyx-sandbox/src/confiner.rs`

**Interfaces:**
- Consumes: `is_bare_pattern` (Task 1), `is_basename_glob_match`
  (already exists in `lib.rs`, private but visible to this submodule).
- Produces: `fn find_basename_glob_matches(root: &Path, deny_paths: &[PathBuf]) -> Vec<PathBuf>`
  (private to `confiner.rs`).

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `crates/aivyx-sandbox/src/confiner.rs`
(after the existing `grant_paths_excluding_returns_empty_when_root_itself_is_denied`
test):

```rust
    #[test]
    fn find_basename_glob_matches_finds_a_nested_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/.env"), "SECRET=1").unwrap();
        std::fs::write(dir.path().join("public.txt"), "hello").unwrap();

        let matches = find_basename_glob_matches(dir.path(), &[PathBuf::from(".env")]);

        assert_eq!(matches, vec![dir.path().join("nested/.env")]);
    }

    #[test]
    fn find_basename_glob_matches_skips_the_scan_entirely_when_no_bare_patterns_are_configured() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "SECRET=1").unwrap();

        // Every entry here has a path separator, so `deny_paths` has no
        // bare patterns at all — the function must return empty without
        // needing to find (or miss) the very real `.env` file present.
        let matches = find_basename_glob_matches(dir.path(), &[PathBuf::from("/some/abs/path")]);

        assert!(matches.is_empty());
    }

    #[test]
    fn find_basename_glob_matches_does_not_follow_a_symlinked_directory() {
        let dir = tempfile::tempdir().unwrap();
        let real_target = tempfile::tempdir().unwrap();
        std::fs::write(real_target.path().join(".env"), "SECRET=1").unwrap();
        std::os::unix::fs::symlink(real_target.path(), dir.path().join("link")).unwrap();

        let matches = find_basename_glob_matches(dir.path(), &[PathBuf::from(".env")]);

        assert!(matches.is_empty(), "must not follow symlinked directories");
    }

    #[tokio::test]
    async fn a_bare_basename_pattern_nested_inside_cwd_is_excluded_from_the_grant() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "SECRET=1").unwrap();
        std::fs::write(dir.path().join("public.txt"), "hello").unwrap();

        let confiner = LandlockConfiner::new(dir.path(), &[], &[PathBuf::from(".env")], true);

        let mut command = tokio::process::Command::new("cat");
        command.arg(dir.path().join(".env"));
        let command = confiner.confine(command);
        let (success, output) = run(command).await;
        assert!(!success, "bare-pattern-matched file should not be readable");
        assert!(!output.contains("SECRET"));

        let mut command = tokio::process::Command::new("cat");
        command.arg(dir.path().join("public.txt"));
        let command = confiner.confine(command);
        let (success, output) = run(command).await;
        assert!(success, "non-matching sibling should still be readable");
        assert!(output.contains("hello"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-sandbox --test-threads=1`
Expected: compile error — `find_basename_glob_matches` does not exist
yet.

- [ ] **Step 3: Implement `find_basename_glob_matches`**

In `crates/aivyx-sandbox/src/confiner.rs`, add these two functions
directly after `grant_paths_excluding` (before `fn build_seccomp_filter`):

```rust
/// Recursively finds every path under `root` whose basename matches a
/// bare (single-component) `deny_paths` pattern — the concrete
/// file-level exclusions `LandlockConfiner::new` needs before granting
/// `root`, since `grant_paths_excluding` only understands specific
/// absolute paths to carve out, not "matches anywhere" patterns. Returns
/// immediately without touching the filesystem if `deny_paths` has no
/// bare entries at all.
fn find_basename_glob_matches(root: &Path, deny_paths: &[PathBuf]) -> Vec<PathBuf> {
    let bare_patterns: Vec<PathBuf> = deny_paths
        .iter()
        .filter(|p| crate::is_bare_pattern(p))
        .cloned()
        .collect();
    if bare_patterns.is_empty() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    walk_for_basename_matches(root, &bare_patterns, &mut matches);
    matches
}

fn walk_for_basename_matches(dir: &Path, bare_patterns: &[PathBuf], matches: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if bare_patterns
            .iter()
            .any(|pattern| crate::is_basename_glob_match(&path, pattern))
        {
            matches.push(path);
            continue; // matched — no need to recurse further into it
        }
        // `file_type()` reflects the entry itself, not a symlink's
        // target, so a symlinked directory is never recursed into —
        // this is what keeps a symlink cycle from causing unbounded
        // recursion here (unlike `grant_paths_excluding`, this function
        // recurses into *every* subdirectory by default, so this guard
        // matters more here).
        if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            walk_for_basename_matches(&path, bare_patterns, matches);
        }
    }
}
```

`is_bare_pattern` and `is_basename_glob_match` are both private
functions defined at the crate root (`lib.rs`) — Rust's privacy rules
make a private item visible to the module it's defined in and all of
that module's descendants, and `confiner` is a child module of the crate
root (`mod confiner;` in `lib.rs`), so `crate::is_bare_pattern(...)` and
`crate::is_basename_glob_match(...)` are already callable from
`confiner.rs` with no visibility changes needed anywhere.

- [ ] **Step 4: Run the new function's tests to verify they pass**

Run: `cargo test -p aivyx-sandbox --test-threads=1 find_basename_glob_matches`
Expected: the three `find_basename_glob_matches_*` tests pass. The
fourth test (`a_bare_basename_pattern_nested_inside_cwd_is_excluded_from_the_grant`)
will still fail at this point — `LandlockConfiner::new` doesn't call
`find_basename_glob_matches` yet. That's expected; it's addressed in the
next step.

- [ ] **Step 5: Wire `find_basename_glob_matches` into `LandlockConfiner::new`**

In `crates/aivyx-sandbox/src/confiner.rs`, replace the body of
`LandlockConfiner::new` from (starting right after the
`detect_landlock_abi` warning block):

```rust
        // `grant_paths_excluding` is applied uniformly to every candidate
        // root, not just `cwd` (any of them could, in principle, contain a
        // nested `deny_paths` entry (most concretely: `cwd` is very
        // commonly itself a subdirectory of the system tmp dir, e.g. in
        // tests or scratch working directories, so the tmp-dir grant below
        // needs the same treatment or it silently re-grants whatever `cwd`'s
        // own carve-out just excluded). It's a no-op (returns the root
        // unchanged) whenever nothing is actually nested underneath, so this
        // costs nothing extra in the common case.
        let mut read_candidates: Vec<PathBuf> =
            DEFAULT_READ_PATHS.iter().map(PathBuf::from).collect();
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            read_candidates.extend(DEFAULT_HOME_READ_PATHS.iter().map(|p| home.join(p)));
        }
        read_candidates.push(cwd.to_path_buf());
        read_candidates.extend(extra_read_paths.iter().cloned());
        let mut read_paths: Vec<PathBuf> = read_candidates
            .iter()
            .flat_map(|root| grant_paths_excluding(root, deny_paths))
            .collect();
        // Read side of the device grants below (read and write rules are
        // separate Landlock rule sets, so both lists need the entries).
        read_paths.extend(DEVICE_RW_PATHS.iter().map(PathBuf::from));

        let mut write_candidates = vec![cwd.to_path_buf(), std::env::temp_dir()];
        if let Some(tmpdir) = std::env::var_os("TMPDIR") {
            write_candidates.push(PathBuf::from(tmpdir));
        }
        let mut write_paths: Vec<PathBuf> = write_candidates
            .iter()
            .flat_map(|root| grant_paths_excluding(root, deny_paths))
            .collect();
        // Individual device files, not subject to deny_paths carve-outs
        // (they're fixed, well-known, and content-free); `path_beneath_rules`
        // silently skips any that don't exist.
        write_paths.extend(DEVICE_RW_PATHS.iter().map(PathBuf::from));
```

with:

```rust
        // Fixed system/toolchain read paths: never scanned for
        // basename-glob matches (see `find_basename_glob_matches`'s doc
        // comment for why — no project secret plausibly lives under
        // `/usr` etc., and recursively walking them would be substantial,
        // pointless work). Bare patterns already have zero effect on
        // `grant_paths_excluding`'s existing absolute-only checks, so
        // passing the plain `deny_paths` list here is a correctness
        // no-op, not a special case that needs its own logic.
        let mut system_read_candidates: Vec<PathBuf> =
            DEFAULT_READ_PATHS.iter().map(PathBuf::from).collect();
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            system_read_candidates.extend(DEFAULT_HOME_READ_PATHS.iter().map(|p| home.join(p)));
        }
        let mut read_paths: Vec<PathBuf> = system_read_candidates
            .iter()
            .flat_map(|root| grant_paths_excluding(root, deny_paths))
            .collect();

        // `cwd` is project-relevant — its own basename-glob matches are
        // computed once here and reused for both its read grant (below)
        // and its write grant (further down), since a project's own
        // secrets are exactly what this feature exists to protect.
        let mut cwd_deny_paths = deny_paths.to_vec();
        cwd_deny_paths.extend(find_basename_glob_matches(cwd, deny_paths));
        read_paths.extend(grant_paths_excluding(cwd, &cwd_deny_paths));

        // `extra_read_paths` entries are also project-relevant (they're
        // explicitly configured additional read locations), so each gets
        // its own basename-glob scan.
        for extra_root in extra_read_paths {
            let mut extra_deny_paths = deny_paths.to_vec();
            extra_deny_paths.extend(find_basename_glob_matches(extra_root, deny_paths));
            read_paths.extend(grant_paths_excluding(extra_root, &extra_deny_paths));
        }
        // Read side of the device grants below (read and write rules are
        // separate Landlock rule sets, so both lists need the entries).
        read_paths.extend(DEVICE_RW_PATHS.iter().map(PathBuf::from));

        let mut write_paths: Vec<PathBuf> = grant_paths_excluding(cwd, &cwd_deny_paths);
        // The OS temp directory and `TMPDIR` are not project-relevant —
        // same reasoning as the system read paths above.
        let mut system_write_candidates = vec![std::env::temp_dir()];
        if let Some(tmpdir) = std::env::var_os("TMPDIR") {
            system_write_candidates.push(PathBuf::from(tmpdir));
        }
        write_paths.extend(
            system_write_candidates
                .iter()
                .flat_map(|root| grant_paths_excluding(root, deny_paths)),
        );
        // Individual device files, not subject to deny_paths carve-outs
        // (they're fixed, well-known, and content-free); `path_beneath_rules`
        // silently skips any that don't exist.
        write_paths.extend(DEVICE_RW_PATHS.iter().map(PathBuf::from));
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p aivyx-sandbox --test-threads=1`
Expected: all tests pass, including all four new tests from Step 1 and
every pre-existing test in this file (`write_inside_the_granted_root_succeeds`,
`write_outside_the_granted_root_fails`, `read_outside_the_allowlist_fails`,
`read_of_an_allowlisted_path_succeeds`,
`a_normal_command_still_works_under_the_seccomp_filter`,
`deny_paths_entry_nested_inside_cwd_is_excluded_from_the_grant`,
`grant_paths_excluding_returns_root_unchanged_when_nothing_is_denied`,
`grant_paths_excluding_returns_empty_when_root_itself_is_denied`) —
proving the restructuring didn't change behavior for absolute-path
entries.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-sandbox/src/confiner.rs
git commit -m "Extend Landlock grants for cwd/extra_read_paths to honor basename-glob deny_paths entries"
```

---

### Task 3: `aivyx-repomap` basename-glob matching

**Files:**
- Modify: `crates/aivyx-repomap/Cargo.toml`
- Modify: `crates/aivyx-repomap/src/lib.rs`

**Interfaces:**
- Produces: nothing new externally visible — `is_denied` (private,
  already used only within this crate) gains basename-glob matching.

- [ ] **Step 1: Add the `globset` dependency**

Edit `crates/aivyx-repomap/Cargo.toml`, adding this line to
`[dependencies]` as the first entry (alphabetically before `ignore`):

```toml
globset = "0.4.18"
```

- [ ] **Step 2: Write the failing test**

Add to the `mod tests` block in `crates/aivyx-repomap/src/lib.rs` (after
the existing `denied_and_gitignored_files_stay_out_of_the_map` test):

```rust
    #[test]
    fn a_bare_basename_pattern_excludes_a_matching_file_from_the_map() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "visible.rs", "pub fn visible_fn() {}\n");
        write(dir.path(), "secret_config.rs", "pub fn secret_fn() {}\n");

        let deny = vec![PathBuf::from("secret_*.rs")];
        let map = RepoMap::new(dir.path().to_path_buf(), deny);
        let rendered = map.render(10_000).unwrap();

        assert!(rendered.contains("visible_fn"));
        assert!(
            !rendered.contains("secret_fn"),
            "bare-pattern-matched file leaked:\n{rendered}"
        );
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p aivyx-repomap --test-threads=1 a_bare_basename_pattern_excludes_a_matching_file_from_the_map`
Expected: FAIL — `secret_fn` appears in the rendered output, since
today's `is_denied` only does `starts_with` against an absolute path and
`"secret_*.rs"` never matches as a prefix of anything.

- [ ] **Step 4: Implement basename-glob matching in `is_denied`**

In `crates/aivyx-repomap/src/lib.rs`, replace:

```rust
/// Denies a path if it (or a symlink-resolved alias) sits under one of
/// `deny_paths`' absolute/tilde-prefixed entries.
///
/// **Known gap (2026-07-29, found at the deny_paths basename-glob
/// feature's final review):** this does NOT support the basename-glob
/// matching `aivyx_sandbox::path_is_denied` added for bare entries like
/// `.env`/`*.pem` — this crate is deliberately zero-dependency on every
/// other workspace crate, so it can't call that function or add
/// `globset`. A bare pattern here is compared via `starts_with` against
/// an absolute path and will practically never match, so a repo-map-parsed
/// source file matching a user's own bare `deny_paths` pattern would still
/// have its symbols/signatures reach the system prompt. Lower severity
/// than the file-read surface `path_is_denied` protects (only
/// signatures reach the prompt here, not file content), and no *default*
/// deny_paths entry has a repomap-parsed extension (`.rs`/`.py`/`.js`/
/// `.jsx`/`.ts`/`.tsx`), so this wasn't treated as merge-blocking — see
/// `ROADMAP.md`'s backlog for the tracked follow-up.
fn is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool {
    // Denied entries are canonicalized at config load; canonicalize the
    // candidate too so a symlinked spelling can't slip past the comparison
    // (same convention as the search tools).
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    deny_paths
        .iter()
        .any(|denied| canonical.starts_with(denied) || path.starts_with(denied))
}
```

with:

```rust
/// Denies a path if it (or a symlink-resolved alias) sits under one of
/// `deny_paths`' absolute/tilde-prefixed entries, or matches a bare
/// basename-glob entry (e.g. `.env`, `*.pem`) by file name.
///
/// **Fixed 2026-07-29** (previously a known gap, found at the
/// `deny_paths` basename-glob feature's own final review): this crate
/// has zero dependencies on any *other workspace crate* (deliberately —
/// keeps repo-map extraction a pure, independently-testable
/// string-in/string-out component, untangled from the security/tools
/// layer), which means it cannot call `aivyx_sandbox::path_is_denied`
/// directly or depend on `aivyx-sandbox`. It can, however, depend on
/// external crates like any other — `globset` (already used elsewhere
/// in the workspace) is now one, alongside the `ignore`/`tree-sitter-*`
/// crates this crate already pulled in. The classification and matching
/// logic below is therefore a deliberate, justified duplicate of
/// `path_is_denied`'s, not an oversight.
fn is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool {
    // Denied entries are canonicalized at config load; canonicalize the
    // candidate too so a symlinked spelling can't slip past the comparison
    // (same convention as the search tools).
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    deny_paths.iter().any(|denied| {
        if is_bare_pattern(denied) {
            is_basename_glob_match(&canonical, denied) || is_basename_glob_match(path, denied)
        } else {
            canonical.starts_with(denied) || path.starts_with(denied)
        }
    })
}

fn is_bare_pattern(path: &Path) -> bool {
    path.parent() == Some(Path::new(""))
}

fn is_basename_glob_match(path: &Path, pattern: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some(pattern) = pattern.to_str() else {
        return false;
    };
    globset::Glob::new(pattern)
        .map(|glob| glob.compile_matcher().is_match(name))
        .unwrap_or(false)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p aivyx-repomap --test-threads=1`
Expected: all tests pass, including the new
`a_bare_basename_pattern_excludes_a_matching_file_from_the_map` test and
the pre-existing `denied_and_gitignored_files_stay_out_of_the_map` test
(proving absolute/tilde-prefixed-entry behavior is completely
unchanged).

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-repomap/Cargo.toml crates/aivyx-repomap/src/lib.rs
git commit -m "Add basename-glob matching to aivyx-repomap's own is_denied"
```

---

### Task 4: Documentation and backlog closure

**Files:**
- Modify: `README.md`
- Modify: `ROADMAP.md`
- Modify: `docs/HISTORY.md`

**Interfaces:**
- Consumes: nothing (documentation only); should be the last task since
  it describes the finished feature.

- [ ] **Step 1: Correct README's Landlock command-tool caveat**

In `README.md`, find this existing text (in the "### 1. `deny_paths` — a
hard block" section):

```markdown
- **Command tools** (`run_command`/`run_shell`): for a path-separator entry,
  `deny_paths` is enforced at the **kernel** level — the Landlock sandbox
  (below) simply never grants access to a denied path, so a shell command
  physically cannot read or write it regardless of how the command is phrased
  (redirection, env vars, etc.). **Basename-glob entries (no separator, e.g.
  `.env`, `*.pem`) are not yet enforced at this layer** — Landlock's grant
  construction (`grant_paths_excluding` in `confiner.rs`) only understands
  fixed-path carve-outs, so a confined command can still read a file matching
  a bare pattern. This is a known, tracked gap (see `ROADMAP.md`'s backlog) —
  the basename-glob feature's primary protection is against the model's own
  auto-allowed `read_file`/`grep`/`glob` calls (see tier 2 below), which *are*
  fully covered.
```

Replace it with:

```markdown
- **Command tools** (`run_command`/`run_shell`): for a path-separator entry,
  `deny_paths` is enforced at the **kernel** level — the Landlock sandbox
  (below) simply never grants access to a denied path, so a shell command
  physically cannot read or write it regardless of how the command is phrased
  (redirection, env vars, etc.). Basename-glob entries (no separator, e.g.
  `.env`, `*.pem`) are enforced too, but only under the working directory
  and any configured `extra_read_paths` — `LandlockConfiner` resolves them
  to concrete file paths there once at startup (the roots a project's own
  secrets could plausibly live under) and carves those out the same way.
  The fixed system paths (`/usr`, `/lib`, etc.) and the OS temp directory
  are deliberately not scanned for basename-glob matches — recursively
  walking them for a project-local pattern would be substantial, pointless
  work — and a file created there, or matching a bare pattern *after*
  startup inside an already-granted directory, isn't retroactively
  excluded either, since Landlock rulesets are static once built.
```

- [ ] **Step 2: Update ROADMAP.md**

In `ROADMAP.md`, delete this entire paragraph (in the "## Backlog"
section — it is currently the last content in the file):

```markdown
**Found at the deny_paths basename-glob feature's final whole-branch
review (2026-07-29), logged rather than expanding that feature's scope
mid-review**: basename-glob `deny_paths` entries (`.env`, `*.pem`, etc.)
are enforced for the model's own file/search/git tools, but not yet for
two other surfaces. (1) **Landlock command-tool grants**:
`grant_paths_excluding` (`crates/aivyx-sandbox/src/confiner.rs`) only
understands fixed-path carve-outs, so a confined `run_shell`/`run_command`
child can still read a file matching a bare pattern — closing this needs
real design work (translating a "matches anywhere" pattern into concrete
filesystem grants at confiner-construction time, e.g. scanning a granted
root for matching basenames the way nested absolute-path denials are
already carved out). (2) **`aivyx-repomap`'s own duplicate `is_denied`**
(a third copy never accounted for during that feature's design, since this
crate is deliberately zero-dependency on every other workspace crate) has
no basename-glob awareness either, though no *default* deny_paths entry
has a repomap-parsed extension, so current impact is nil. Both are
documented as known limitations in the affected code and in `README.md`
(the Landlock one) rather than left silently wrong.
```

After deletion, the "## Backlog" section's last paragraph is "No new
capability opportunities were found in test quality or the
security/gate-tier-order/Landlock dimension this pass — see
`docs/HISTORY.md`'s '2026-07-28 capability audit' chapter for the full
account of what was checked." — leave that paragraph as the new end of
the file.

Then, in the "## Current status" section, find this existing text (the
end of the `delegate_task` REPL isolation entry):

```markdown
composition. Sub-agents keep `run_command`/`run_shell` for one-shot
needs; only interactive multi-turn REPL sessions are unavailable to them.

See `docs/HISTORY.md` for the full phase-by-phase narrative behind
every item above.
```

Insert a new "shipped" paragraph between them:

```markdown
composition. Sub-agents keep `run_command`/`run_shell` for one-shot
needs; only interactive multi-turn REPL sessions are unavailable to them.

**Landlock + `aivyx-repomap` basename-glob enforcement — shipped.** The
last open item from the `deny_paths` basename-glob feature's own final
review, closing the entire 2026-07-28 audit lineage's backlog: Landlock's
command-tool grants (`grant_paths_excluding` in
`crates/aivyx-sandbox/src/confiner.rs`) and `aivyx-repomap`'s own
duplicate `is_denied` didn't understand basename-glob `deny_paths`
entries (`.env`, `*.pem`, etc.), so a confined `run_shell`/`run_command`
child could still read a matching file, and a repo-map-parsed source file
matching a user's own bare pattern could still have its symbols reach the
system prompt. Fixed for Landlock by resolving bare patterns into
concrete file paths, once at session startup, scoped to the working
directory and any configured `extra_read_paths` (not the fixed system
paths or the OS temp directory — scanning those for a project-local
pattern would be substantial, pointless work) — the existing, well-tested
carve-out algorithm (`grant_paths_excluding`) needed zero changes,
receiving the resolved concrete paths exactly like any other denial.
Fixed for `aivyx-repomap` by adding `globset` as a new dependency and
mirroring the canonical matcher's logic locally — a deliberate, justified
duplicate this time, since the crate's real architectural boundary (zero
dependency on *other workspace crates*, not zero external dependencies at
all) stays intact.

See `docs/HISTORY.md` for the full phase-by-phase narrative behind
every item above.
```

Also update the `_Last updated:_` line at the top of `ROADMAP.md` from
`2026-07-29` to today's actual date.

- [ ] **Step 3: Add a HISTORY.md chapter**

In `docs/HISTORY.md`, append a new chapter after the "### `delegate_task`
REPL isolation — ✅ shipped" chapter (the last chapter in the file):

```markdown
### Landlock + `aivyx-repomap` basename-glob enforcement — ✅ shipped

The last open item from the `deny_paths` basename-glob feature's own
final whole-branch review (2026-07-29), closing the entire 2026-07-28
capability-audit lineage's backlog with nothing left tracked. Design spec
at
`docs/superpowers/specs/2026-07-29-landlock-repomap-basename-glob-design.md`,
implementation plan at
`docs/superpowers/plans/2026-07-29-landlock-repomap-basename-glob.md`,
executed via `subagent-driven-development`.

**The problem**: basename-glob `deny_paths` entries were enforced for
the model's own file/search/git tools via
`aivyx_sandbox::path_is_denied`, but two other surfaces never got the
same treatment. (1) Landlock's command-tool grants
(`grant_paths_excluding` in `crates/aivyx-sandbox/src/confiner.rs`) only
understood fixed-path carve-outs — a bare pattern like `.env` never
matches an absolute grant root via `starts_with`, so it was silently
never carved out, and a confined `run_shell`/`run_command` child could
still read (or, given this project's unrestricted-network-for-approved-commands
known limitation, exfiltrate) a file matching one. (2)
`aivyx-repomap`'s own duplicate `is_denied` — a third copy never
accounted for during the `deny_paths` feature's own design — had no
basename-glob awareness either, so a repo-map-parsed source file
matching a user's bare pattern could still have its symbols/signatures
reach the system prompt.

**A factual correction made during design, before any code was
written**: the original backlog entry claimed `aivyx-repomap` "can't add
`globset`" because it's "deliberately zero-dependency." Checking its
actual `Cargo.toml` showed this crate already depends on several
external crates (`ignore`, `tree-sitter` and three per-language
grammars) — its real, deliberate architectural boundary is zero
dependency on *other workspace crates* specifically (keeping repo-map
extraction a pure, independently-testable string-in/string-out
component, untangled from the security/tools layer), not zero external
dependencies in general. Adding `globset` — already used at the same
version elsewhere in the workspace — doesn't touch that boundary at all.
**General lesson: a "can't do X because of constraint Y" claim made
during a fast-moving final review is worth re-verifying against the
actual code before it hardens into the next feature's starting
assumption** — this one would have sent an entire design down the wrong
path (hand-rolling a matcher, or debating whether to break the
boundary) if taken at face value.

**Landlock fix**: rather than modifying the existing, well-tested
`grant_paths_excluding` recursion at all, a new step
(`find_basename_glob_matches`) runs before it, only for
project-relevant grant roots — the working directory (both its read and
write grant) and each configured `extra_read_paths` entry (read only) —
recursively walking the directory once with `std::fs::read_dir` (no new
dependency, matching this file's existing hand-rolled-recursion style)
to resolve any bare pattern into the concrete absolute paths it actually
matches there, then merging those into that root's own deny list before
calling the unchanged `grant_paths_excluding`. The fixed system paths
(`/usr`/`/lib`/`/etc`/home-toolchain dirs) and the OS temp directory
continue receiving the plain `deny_paths` list unmodified — bare
patterns already have zero effect on `grant_paths_excluding`'s
absolute-only checks, so this is a correctness no-op for those roots,
not a special case needing its own logic. Confirmed via `agent_builder.rs`'s
single call site that `LandlockConfiner::new` is constructed exactly once
per session (not per command, wrapped in `Arc<dyn ExecutionConfiner>` and
reused for every subsequent spawn), making the one-time recursive scan a
bounded, session-startup cost proportional to project size — the same
cost category `aivyx-repomap`'s own one-time project walk already
accepts, not a new performance risk.

**A new shared predicate, `is_bare_pattern`**, was extracted from
`path_is_denied`'s previously-inline single-component classification
check, reused by both `path_is_denied` and the new
`find_basename_glob_matches` — avoiding two independent call sites that
must agree on the same classification, the exact duplication shape a
task reviewer had already flagged as a theoretical risk during the
original `deny_paths` feature.

**`aivyx-repomap` fix**: added `globset` as a new dependency and gave its
own `is_denied` the identical classification + basename-glob matching
logic `path_is_denied` already has — a deliberate, justified duplicate
this time (see the factual correction above), not an oversight left
unfixed.

**Deliberately out of scope, documented rather than solved**: a file
matching a bare pattern created *after* `LandlockConfiner::new` runs,
inside a directory that was granted wholesale at startup because nothing
matched yet, isn't retroactively excluded — Landlock rulesets are static
once built, and closing this would mean rebuilding the ruleset (and
re-walking the project) on every command spawn, reintroducing the exact
per-command performance cost the once-per-session design avoids, for a
narrow race window.

With this shipped, the entire 2026-07-28 capability-audit backlog
lineage is closed — no tracked items remain in `ROADMAP.md`.
```

- [ ] **Step 4: Commit**

```bash
git add README.md ROADMAP.md docs/HISTORY.md
git commit -m "Document Landlock + aivyx-repomap basename-glob enforcement; close the backlog"
```
