# `deny_paths` Basename-Glob Matching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a `deny_paths` entry with no path separator (e.g. `.env`,
`*.pem`) match any file with that basename anywhere, instead of requiring
a fixed absolute path per project — closing the gap where a project-local
secret file has no default protection.

**Architecture:** `aivyx_sandbox::path_is_denied` classifies each entry by
component count: a single-component entry is a basename-glob pattern
(matched against `path.file_name()` via `globset`); everything else keeps
today's `starts_with` prefix match. `aivyx-config`'s
`resolved_deny_paths()` skips tilde/symlink resolution for bare entries so
they reach `path_is_denied` as literal single-component patterns instead
of being accidentally resolved against whatever the process happened to
be launched from. A pre-existing duplicate of the matching logic in
`aivyx-tools` (used by `grep`/`glob`/`move_file`/`git_commit`'s own
directory-walk checks) is deleted in favor of calling the one canonical
function directly.

**Tech Stack:** Rust, `globset` (already a workspace dependency via
`aivyx-tools`, newly added to `aivyx-sandbox`).

## Global Constraints

- No new TOML schema or config field — `deny_paths: Vec<String>` accepts
  both shapes (absolute/tilde path, or bare basename-glob pattern),
  distinguished only by the presence of a path separator.
- `aivyx_sandbox::path_is_denied`'s signature —
  `fn path_is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool` — must
  not change. Every existing caller (`ConfirmationGate::check`,
  `LandlockConfiner`'s grant construction, `agent/mod.rs`'s autonomous
  worktree-boundary check) must keep compiling and behaving identically
  for every existing (path-separator-containing) entry.
- A bare entry is classified by **the raw config string**, not by
  presence of glob metacharacters — `.env` (no wildcards) and `*.pem`
  (has one) are both bare patterns as long as neither contains `/` and
  neither starts with `~`.
- An entry starting with `~` (including the bare `"~"` alone) is **never**
  treated as a bare basename-glob pattern, even though `"~"` by itself
  contains no `/` — it must keep going through today's tilde-expansion
  path. Classification rule, used identically in both places it appears:
  `is_bare_pattern(raw) == !raw.starts_with('~') && !raw.contains('/')`.
- Existing tests `tilde_prefixed_deny_paths_expand_to_the_home_directory`,
  `non_tilde_deny_paths_pass_through_unchanged`, and
  `tilde_username_syntax_is_skipped_not_treated_as_literal` (all in
  `crates/aivyx-config/src/lib.rs`) must continue passing unchanged.
- `SandboxSettings::resolved_extra_read_paths` (also in
  `crates/aivyx-config/src/lib.rs`, sharing the `resolve_tilde_paths`
  helper with `resolved_deny_paths`) must not change behavior at all —
  the new bare-pattern classification is added only inside
  `resolved_deny_paths`, never inside the shared `resolve_tilde_paths`
  helper itself.

---

### Task 1: Basename-glob matching in `aivyx_sandbox::path_is_denied`

**Files:**
- Modify: `crates/aivyx-sandbox/Cargo.toml`
- Modify: `crates/aivyx-sandbox/src/lib.rs`

**Interfaces:**
- Produces: `path_is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool`
  (signature unchanged; only its matching semantics for single-component
  entries change). Later tasks depend on this new behavior existing.

- [ ] **Step 1: Add the `globset` dependency**

Edit `crates/aivyx-sandbox/Cargo.toml`, adding this line to the
`[dependencies]` section (alphabetically after `directories`):

```toml
globset = "0.4.18"
```

- [ ] **Step 2: Write the failing tests**

Add to the `mod tests` block at the bottom of
`crates/aivyx-sandbox/src/lib.rs` (after the existing
`autonomous_mode_clones_share_state` test, still inside the same `mod
tests { use super::*; ... }`):

```rust
    #[test]
    fn a_bare_basename_matches_the_same_name_under_any_directory() {
        let deny_paths = vec![PathBuf::from(".env")];
        assert!(path_is_denied(Path::new("/project-a/.env"), &deny_paths));
        assert!(path_is_denied(
            Path::new("/project-b/nested/.env"),
            &deny_paths
        ));
        assert!(!path_is_denied(
            Path::new("/project-a/.env.example"),
            &deny_paths
        ));
    }

    #[test]
    fn a_bare_glob_pattern_matches_by_wildcard() {
        let deny_paths = vec![PathBuf::from("*.pem")];
        assert!(path_is_denied(Path::new("/any/dir/server.pem"), &deny_paths));
        assert!(!path_is_denied(Path::new("/any/dir/.env"), &deny_paths));
    }

    #[test]
    fn a_bare_pattern_does_not_match_a_substring_of_a_longer_basename() {
        let deny_paths = vec![PathBuf::from(".env")];
        // A directory or file merely containing the pattern text is not a
        // match — glob matching is exact against the whole basename, not
        // a substring search.
        assert!(!path_is_denied(
            Path::new("/project/.env-backup"),
            &deny_paths
        ));
    }

    #[test]
    fn a_path_separator_entry_keeps_exact_prefix_matching() {
        let deny_paths = vec![PathBuf::from("/home/user/.ssh")];
        assert!(path_is_denied(
            Path::new("/home/user/.ssh/id_rsa"),
            &deny_paths
        ));
        assert!(!path_is_denied(
            Path::new("/home/user/.ssh-backup/id_rsa"),
            &deny_paths
        ));
        assert!(!path_is_denied(Path::new("/home/user/other"), &deny_paths));
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p aivyx-sandbox --test-threads=1`
Expected: FAIL — the four new tests
(`a_bare_basename_matches_the_same_name_under_any_directory`,
`a_bare_glob_pattern_matches_by_wildcard`,
`a_bare_pattern_does_not_match_a_substring_of_a_longer_basename`,
`a_path_separator_entry_keeps_exact_prefix_matching`) reference behavior
(`.env`/`*.pem` matching anywhere) that `path_is_denied` doesn't
implement yet. Run the full crate suite with no name filter here — none
of the four new test names share a common substring, so a positional
filter would not select all of them at once.

- [ ] **Step 4: Implement the matching logic**

In `crates/aivyx-sandbox/src/lib.rs`, replace:

```rust
/// Shared by `ConfirmationGate::is_denied` and (behind `sandbox-backend`)
/// `LandlockConfiner`'s path-grant construction — both need the same
/// "is this path under a denied path" check.
pub fn path_is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool {
    deny_paths.iter().any(|denied| path.starts_with(denied))
}
```

with:

```rust
/// Shared by `ConfirmationGate::is_denied` and (behind `sandbox-backend`)
/// `LandlockConfiner`'s path-grant construction — both need the same
/// "is this path under a denied path" check.
///
/// A `deny_paths` entry with a single path component (e.g. `.env`,
/// `*.pem`) is a basename-glob pattern, matched against `path`'s own file
/// name wherever it appears — not just at one fixed location. Every
/// other entry keeps the original exact-prefix `starts_with` check.
/// Classifying by component count (rather than a config-time flag) means
/// this function's signature never has to change: an entry only has a
/// single component in the first place when `aivyx-config`'s
/// `resolved_deny_paths` deliberately left it unresolved for exactly this
/// reason (see that function's own doc comment).
pub fn path_is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool {
    deny_paths.iter().any(|denied| {
        if denied.parent() == Some(Path::new("")) {
            is_basename_glob_match(path, denied)
        } else {
            path.starts_with(denied)
        }
    })
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

Add `use globset::Glob;`? No — the code above calls
`globset::Glob::new(...)` fully qualified, so no new `use` line is
needed; leave the existing `use` block at the top of
`crates/aivyx-sandbox/src/lib.rs` untouched.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p aivyx-sandbox --test-threads=1`
Expected: all tests pass, including the four new ones.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-sandbox/Cargo.toml crates/aivyx-sandbox/src/lib.rs
git commit -m "Add basename-glob matching to path_is_denied"
```

---

### Task 2: `aivyx-config` skips resolution for bare basename-glob entries

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs`

**Interfaces:**
- Consumes: nothing new from Task 1 (this task's tests exercise
  `resolved_deny_paths()`'s output shape directly, not `path_is_denied`).
- Produces: `PermissionSettings::resolved_deny_paths()` (signature
  unchanged: `-> Vec<PathBuf>`), now returning bare entries as literal,
  unresolved single-component `PathBuf`s instead of attempting
  tilde/symlink resolution on them.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `crates/aivyx-config/src/lib.rs`, near
the existing `tilde_prefixed_deny_paths_expand_to_the_home_directory`/
`non_tilde_deny_paths_pass_through_unchanged` tests:

```rust
    #[test]
    fn a_bare_basename_glob_entry_is_left_unresolved() {
        let settings = PermissionSettings {
            deny_paths: vec![".env".to_string(), "*.pem".to_string()],
            ..PermissionSettings::default()
        };

        let resolved = settings.resolved_deny_paths();
        assert!(resolved.contains(&PathBuf::from(".env")));
        assert!(resolved.contains(&PathBuf::from("*.pem")));
    }

    #[test]
    fn a_bare_entry_is_unaffected_by_the_process_current_directory() {
        // Regression test for the bug this fix closes: if a bare entry
        // were run through tilde/symlink resolution like a real path,
        // `canonicalize` could silently rewrite it into an absolute path
        // tied to wherever the process happened to be launched from —
        // breaking the "matches this basename anywhere" semantic. Proven
        // here by asserting the entry stays the literal configured
        // string regardless of what the test process's own cwd contains
        // (this test does not need to create a real `.env` file or
        // change directory to prove it — the fix means resolution is
        // never attempted at all for a bare entry).
        let settings = PermissionSettings {
            deny_paths: vec![".env".to_string()],
            ..PermissionSettings::default()
        };

        assert_eq!(settings.resolved_deny_paths(), vec![PathBuf::from(".env")]);
    }

    #[test]
    fn a_bare_tilde_alone_still_expands_to_the_home_directory() {
        // `"~"` contains no `/`, so it must be special-cased in the
        // bare-pattern classification — otherwise this would regress from
        // an already-supported case into a literal, wrong pattern.
        let home = directories::UserDirs::new()
            .unwrap()
            .home_dir()
            .canonicalize()
            .expect("$HOME must exist");
        let settings = PermissionSettings {
            deny_paths: vec!["~".to_string()],
            ..PermissionSettings::default()
        };

        assert_eq!(settings.resolved_deny_paths(), vec![home]);
    }
```

- [ ] **Step 2: Run the tests to confirm the current (pre-fix) result**

Run: `cargo test -p aivyx-config --test-threads=1`
Expected: all three tests **pass already**, even before Step 3's fix.
This was confirmed empirically before writing this plan: today's
`resolve_symlinks` canonicalizes `.env` against the test process's
actual working directory, that directory has no file literally named
`.env`, canonicalize fails, and the walk-up logic bottoms out returning
the original string unchanged — so a bare entry already happens to
survive unresolved by accident, purely because no coincidentally-named
file exists at the test's cwd. This is exactly the fragile,
environment-dependent behavior the fix in Step 3 replaces with a
guarantee: after the fix, a bare entry is *never even attempted* to be
resolved, regardless of what exists on disk anywhere. There is no
red/green cycle for this task in the traditional sense — these tests are
a regression guard pinning down behavior that must remain true by
construction, not one that happens to hold today by coincidence. Confirm
all three pass now, then confirm again in Step 4 that they still pass
after the fix, and treat any change in outcome between the two runs as a
sign something was misunderstood.

- [ ] **Step 3: Implement the fix**

In `crates/aivyx-config/src/lib.rs`, replace:

```rust
impl PermissionSettings {
    /// Expands a leading `~` (home directory) in each `deny_paths` entry
    /// into an absolute `PathBuf`. Entries that can't be expanded (no home
    /// directory found) are skipped rather than left unresolved and
    /// silently wrong.
    pub fn resolved_deny_paths(&self) -> Vec<PathBuf> {
        resolve_tilde_paths(&self.deny_paths)
    }
}
```

with:

```rust
impl PermissionSettings {
    /// Expands a leading `~` (home directory) in each path-like
    /// `deny_paths` entry into an absolute `PathBuf`. A *bare* entry —
    /// one with no `/` and not starting with `~` (e.g. `.env`, `*.pem`)
    /// — is left exactly as configured instead: `aivyx_sandbox::path_is_denied`
    /// treats a single-component entry as a basename-glob pattern matched
    /// against a path's file name, not a real filesystem location to
    /// resolve. Running it through tilde-expansion and
    /// symlink-canonicalization here would tie a "matches anywhere"
    /// pattern to whatever the process's actual launch directory happens
    /// to contain instead. Path-like entries that can't be expanded (no
    /// home directory found) are skipped rather than left unresolved and
    /// silently wrong.
    pub fn resolved_deny_paths(&self) -> Vec<PathBuf> {
        let (bare, path_like): (Vec<String>, Vec<String>) = self
            .deny_paths
            .iter()
            .cloned()
            .partition(|raw| !raw.starts_with('~') && !raw.contains('/'));
        let mut resolved = resolve_tilde_paths(&path_like);
        resolved.extend(bare.into_iter().map(PathBuf::from));
        resolved
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-config --test-threads=1`
Expected: all tests pass, including the three new ones and the
pre-existing `tilde_prefixed_deny_paths_expand_to_the_home_directory`,
`non_tilde_deny_paths_pass_through_unchanged`, and
`tilde_username_syntax_is_skipped_not_treated_as_literal`.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-config/src/lib.rs
git commit -m "Skip tilde/symlink resolution for bare deny_paths patterns"
```

---

### Task 3: New default `deny_paths` entries

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs`

**Interfaces:**
- Consumes: Task 1's matching behavior (this task's test asserts the
  literal config strings are present in the default list — it does not
  itself exercise matching, so it has no hard runtime dependency on Task
  1, but the entries are meaningless without it).

- [ ] **Step 1: Write the failing test**

In `crates/aivyx-config/src/lib.rs`, extend the existing
`default_deny_paths_covers_common_credential_locations` test (do not add
a parallel test) by adding six more entries to its `for expected in [...]`
list:

```rust
    #[test]
    fn default_deny_paths_covers_common_credential_locations() {
        // Regression test for a full-codebase audit finding: `~/.ssh`/
        // `~/.aws` covered the two most obvious cases, but `read_file`
        // has no OS-level backstop at all (Landlock only wraps spawned
        // child processes, never in-process file reads — see
        // `ConfirmationGate::check`, which auto-allows `ActionKind::Read`
        // unconditionally once past this exact list) — so this list is
        // the *only* protection for reads, and it was missing several
        // other common plaintext-credential locations.
        let deny_paths = PermissionSettings::default().deny_paths;
        for expected in [
            "~/.gnupg",
            "~/.netrc",
            "~/.docker/config.json",
            "~/.kube/config",
            "~/.npmrc",
            "~/.pypirc",
            "~/.config/gcloud",
            "~/.azure",
            "~/.cargo/credentials.toml",
            "~/.config/gh",
            // Basename-glob entries (2026-07-28 capability audit): these
            // recur across arbitrary project directories, unlike the
            // fixed `~/`-anchored entries above, so they need matching
            // by name rather than by one absolute location.
            ".env",
            ".env.*",
            "id_rsa",
            "id_ed25519",
            "*.pem",
            "*.key",
        ] {
            assert!(
                deny_paths.contains(&expected.to_string()),
                "expected default deny_paths to include {expected:?}, got {deny_paths:?}"
            );
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p aivyx-config --test-threads=1 default_deny_paths_covers_common_credential_locations`
Expected: FAIL — the six new entries aren't in the default list yet.

- [ ] **Step 3: Add the new default entries**

In `crates/aivyx-config/src/lib.rs`, extend `PermissionSettings::default()`'s
`deny_paths` vec:

```rust
            deny_paths: vec![
                "~/.ssh".to_string(),
                "~/.aws".to_string(),
                "~/.config/aivyx-coder".to_string(),
                "~/.gnupg".to_string(),
                "~/.netrc".to_string(),
                "~/.docker/config.json".to_string(),
                "~/.kube/config".to_string(),
                "~/.npmrc".to_string(),
                "~/.pypirc".to_string(),
                "~/.config/gcloud".to_string(),
                "~/.azure".to_string(),
                "~/.cargo/credentials.toml".to_string(),
                "~/.config/gh".to_string(),
                ".env".to_string(),
                ".env.*".to_string(),
                "id_rsa".to_string(),
                "id_ed25519".to_string(),
                "*.pem".to_string(),
                "*.key".to_string(),
            ],
```

Also update the doc comment directly above this field (currently ending
"...and package-registry tokens (including this project's own
toolchain's).") by adding one more sentence:

```rust
            // `read_file` has no OS-level backstop at all — Landlock only
            // confines spawned child processes, never this crate's own
            // in-process file reads — so this list is the *sole*
            // protection against the model reading plaintext credentials
            // via a normal, auto-allowed `ActionKind::Read` call. Not
            // exhaustive (impossible to be), but covers the common,
            // high-value cases beyond SSH/AWS: GPG, generic netrc-style
            // creds, container/cluster/cloud-CLI auth, and package-registry
            // tokens (including this project's own toolchain's). The
            // basename-glob entries below (no leading `~`) match by file
            // name anywhere rather than one fixed location, covering
            // project-local secrets like `.env` that recur across
            // arbitrary project directories.
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p aivyx-config --test-threads=1`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-config/src/lib.rs
git commit -m "Add basename-glob entries to the default deny_paths list"
```

---

### Task 4: Consolidate the duplicate `is_denied` in `aivyx-tools`

**Files:**
- Modify: `crates/aivyx-tools/src/path_resolve.rs`
- Modify: `crates/aivyx-tools/src/tools/grep.rs`
- Modify: `crates/aivyx-tools/src/tools/glob.rs`
- Modify: `crates/aivyx-tools/src/tools/move_file.rs`
- Modify: `crates/aivyx-tools/src/tools/git_commit.rs`

**Interfaces:**
- Consumes: `aivyx_sandbox::path_is_denied` (Task 1) — this task makes
  `grep`/`glob`/`move_file`/`git_commit` call it directly instead of a
  local duplicate.
- Produces: no new public interface; this is a pure refactor. Later
  tasks (Task 5) depend on all four tools now sharing Task 1's matching
  logic.

This is a mechanical, behavior-preserving refactor (for every
path-separator-containing entry, the old and new code paths compute
identically) — there is no new test to write first; the existing test
suite in each file is the regression check.

- [ ] **Step 1: Delete the duplicate function**

In `crates/aivyx-tools/src/path_resolve.rs`, delete this function
entirely (including its doc comment):

```rust
/// Mirrors `ConfirmationGate::is_denied`'s exact `starts_with` logic. Needed
/// separately by `grep`/`glob`: their top-level `permission_request` target
/// is the search *root*, which the gate correctly denies if the root itself
/// is under a denied path — but a root that is instead an *ancestor* of a
/// denied path passes that check, so a recursive walk still needs its own
/// per-entry check to avoid silently reading into the denied subtree.
pub(crate) fn is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool {
    deny_paths.iter().any(|denied| path.starts_with(denied))
}
```

- [ ] **Step 2: Update `grep.rs`'s import and call site**

In `crates/aivyx-tools/src/tools/grep.rs`, change:

```rust
use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
```

to:

```rust
use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget, path_is_denied};
```

and change:

```rust
use crate::path_resolve::{is_denied, resolve};
```

to:

```rust
use crate::path_resolve::resolve;
```

and change the call site:

```rust
        if is_denied(path, deny_paths) {
```

to:

```rust
        if path_is_denied(path, deny_paths) {
```

- [ ] **Step 3: Update `glob.rs`'s import and call site**

In `crates/aivyx-tools/src/tools/glob.rs`, apply the identical three
changes as Step 2: extend the `aivyx_sandbox` import with
`path_is_denied`, narrow the `path_resolve` import to just `resolve`, and
rename the call site `is_denied(path, deny_paths)` to
`path_is_denied(path, deny_paths)`.

- [ ] **Step 4: Update `move_file.rs`'s import and call site**

In `crates/aivyx-tools/src/tools/move_file.rs`, apply the identical
three changes: extend the `aivyx_sandbox` import with `path_is_denied`,
narrow the `path_resolve` import to just `resolve`, and rename the call
site `is_denied(path, deny_paths)` to `path_is_denied(path, deny_paths)`.

- [ ] **Step 5: Update `git_commit.rs`'s import and call site**

In `crates/aivyx-tools/src/tools/git_commit.rs`, change:

```rust
use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
```

to:

```rust
use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget, path_is_denied};
```

change:

```rust
use crate::path_resolve::{is_denied, resolve};
```

to:

```rust
use crate::path_resolve::resolve;
```

and change the call site:

```rust
            if is_denied(&path, &self.deny_paths) {
```

to:

```rust
            if path_is_denied(&path, &self.deny_paths) {
```

- [ ] **Step 6: Run the full `aivyx-tools` test suite**

Run: `cargo test -p aivyx-tools --test-threads=1`
Expected: all existing tests still pass, including
`grep.rs`'s `does_not_descend_into_a_denied_subtree_even_as_an_ancestor_root`,
`glob.rs`'s test of the same name,
`move_file.rs`'s `a_directory_move_is_refused_when_a_deny_path_is_nested_inside_it`
and `a_gitignored_deny_path_nested_in_the_directory_is_still_caught`, and
`git_commit.rs`'s `a_denied_explicit_path_is_rejected` — proving the
consolidation is a pure refactor with no behavior change for the
prefix-match case.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-tools/src/path_resolve.rs \
        crates/aivyx-tools/src/tools/grep.rs \
        crates/aivyx-tools/src/tools/glob.rs \
        crates/aivyx-tools/src/tools/move_file.rs \
        crates/aivyx-tools/src/tools/git_commit.rs
git commit -m "Consolidate the duplicate deny_paths matcher onto aivyx_sandbox::path_is_denied"
```

---

### Task 5: Prove bare basename-glob entries are caught by each tool's own walk

**Files:**
- Modify: `crates/aivyx-tools/src/tools/grep.rs`
- Modify: `crates/aivyx-tools/src/tools/glob.rs`
- Modify: `crates/aivyx-tools/src/tools/move_file.rs`
- Modify: `crates/aivyx-tools/src/tools/git_commit.rs`

**Interfaces:**
- Consumes: Task 4's consolidation (these tests would fail without it,
  since they pass a bare `PathBuf` — e.g. `PathBuf::from("id_rsa")` —
  as a `deny_paths` entry, which only `aivyx_sandbox::path_is_denied`
  interprets as a basename-glob pattern).

- [ ] **Step 1: Write `grep.rs`'s new test**

Add to `crates/aivyx-tools/src/tools/grep.rs`'s `mod tests` block, after
`does_not_descend_into_a_denied_subtree_even_as_an_ancestor_root`:

```rust
    #[tokio::test]
    async fn a_bare_basename_deny_paths_entry_blocks_a_nested_match() {
        let dir = tempfile::tempdir().unwrap();
        let secret_dir = dir.path().join("secret");
        std::fs::create_dir(&secret_dir).unwrap();
        std::fs::write(secret_dir.join("id_rsa"), "needle\n").unwrap();
        std::fs::write(dir.path().join("public.txt"), "needle\n").unwrap();

        let tool = GrepTool::new(vec![PathBuf::from("id_rsa")]);
        let output = run(&tool, dir.path(), args("needle", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("public.txt"));
        assert!(!text.contains("id_rsa"));
    }
```

- [ ] **Step 2: Write `glob.rs`'s new test**

Add to `crates/aivyx-tools/src/tools/glob.rs`'s `mod tests` block, after
`does_not_descend_into_a_denied_subtree_even_as_an_ancestor_root`:

```rust
    #[tokio::test]
    async fn a_bare_basename_deny_paths_entry_blocks_a_nested_match() {
        let dir = tempfile::tempdir().unwrap();
        let secret_dir = dir.path().join("secret");
        std::fs::create_dir(&secret_dir).unwrap();
        std::fs::write(secret_dir.join("id_rsa.txt"), "").unwrap();
        std::fs::write(dir.path().join("public.txt"), "").unwrap();

        let tool = GlobTool::new(vec![PathBuf::from("id_rsa.txt")]);
        let output = run(&tool, dir.path(), args("**/*.txt", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("public.txt"));
        assert!(!text.contains("id_rsa.txt"));
    }
```

- [ ] **Step 3: Write `move_file.rs`'s new test**

Add to `crates/aivyx-tools/src/tools/move_file.rs`'s `mod tests` block,
after `a_gitignored_deny_path_nested_in_the_directory_is_still_caught`:

```rust
    #[test]
    fn a_bare_basename_deny_paths_entry_blocks_a_nested_move() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("secrets")).unwrap();
        std::fs::write(dir.path().join("secrets/.env"), "SECRET=1\n").unwrap();

        let tool = MoveFileTool::new(vec![PathBuf::from(".env")]);
        let result = tool.permission_request(
            &json!({ "from": "secrets", "to": "archive/secrets" }),
            dir.path(),
        );

        let Err(ToolError::ExecutionFailed(message)) = result else {
            panic!("expected ExecutionFailed, got {result:?}");
        };
        assert!(message.contains(".env"), "message: {message}");
        assert!(dir.path().join("secrets/.env").exists());
    }
```

- [ ] **Step 4: Write `git_commit.rs`'s new test**

Add to `crates/aivyx-tools/src/tools/git_commit.rs`'s `mod tests` block,
after `a_denied_explicit_path_is_rejected`:

```rust
    #[tokio::test]
    async fn a_bare_basename_deny_paths_entry_rejects_a_matching_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let cwd = dir.path().canonicalize().unwrap();
        std::fs::create_dir(cwd.join("secret")).unwrap();
        std::fs::write(cwd.join("secret/.env"), "TOP-SECRET\n").unwrap();

        let tool = GitCommitTool::new(vec![PathBuf::from(".env")]);
        let result = tool
            .execute(
                json!({ "message": "sneaky", "paths": ["secret/.env"] }),
                &ctx(&cwd),
            )
            .await;
        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tools --test-threads=1`
Expected: all tests pass, including the four new ones. These tests add
regression coverage for behavior Tasks 1 and 4 already implemented (a
bare `deny_paths` entry only means "basename-glob pattern" once
`aivyx_sandbox::path_is_denied` is the function actually doing the
matching) — there is no separate implementation step in this task.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/tools/grep.rs \
        crates/aivyx-tools/src/tools/glob.rs \
        crates/aivyx-tools/src/tools/move_file.rs \
        crates/aivyx-tools/src/tools/git_commit.rs
git commit -m "Add tool-level tests for bare basename-glob deny_paths entries"
```

---

### Task 6: Documentation and backlog closure

**Files:**
- Modify: `README.md`
- Modify: `ROADMAP.md`
- Modify: `docs/HISTORY.md`

**Interfaces:**
- Consumes: nothing (documentation only); should be the last task since
  it describes the finished feature.

- [ ] **Step 1: Update README's `deny_paths` security-model section**

In `README.md`, in the "### 1. `deny_paths` — a hard block" section,
replace:

```markdown
`permissions.deny_paths` (default `~/.ssh`, `~/.aws`) are paths that are never
accessible, checked before any prompt or cache. Entries are `~`-expanded and
**symlink-canonicalized**, and the check matches any path *at or under* a
denied entry. This covers:
```

with:

```markdown
`permissions.deny_paths` (default `~/.ssh`, `~/.aws`, plus several more —
see the config reference below) are paths that are never accessible,
checked before any prompt or cache. An entry containing a `/` (or a bare
`~`) is `~`-expanded and **symlink-canonicalized**, and the check matches
any path *at or under* that denied entry — unchanged from before. An
entry with **no path separator** (e.g. `.env`, `*.pem`) is instead a
**basename-glob pattern**: it matches any file with that name anywhere,
not just one fixed absolute location — useful for a project-local secret
file that recurs across every project directory the agent might be
pointed at, which a fixed absolute path can't express. This covers:
```

- [ ] **Step 2: Update README's config reference example**

In `README.md`, replace:

```toml
[permissions]
deny_paths = ["~/.ssh", "~/.aws"]
max_tool_iterations_per_turn = 25
```

with:

```toml
[permissions]
# A path-separator entry (or a bare "~") is an exact absolute location,
# ~-expanded and symlink-canonicalized. A bare entry with no separator
# (e.g. ".env", "*.pem") is a basename-glob pattern instead, matching any
# file with that name anywhere rather than one fixed location.
deny_paths = ["~/.ssh", "~/.aws", ".env", "*.pem"]
max_tool_iterations_per_turn = 25
```

- [ ] **Step 3: Update ROADMAP.md — remove the backlog item, add a shipped paragraph**

In `ROADMAP.md`, in the "## Backlog" section, the 2026-07-28 audit
paragraph currently reads (intro sentence + two bullets):

```markdown
A fresh audit (2026-07-28) covering the same four dimensions against the
current codebase found two documentation-accuracy gaps, fixed directly
(the autonomous-mode paragraph was missing `repl_start`/MCP-tool/
`remember_preference` denials added since it was written; the TOCTOU
known-limitation bullet was missing `patch_file`, which has the identical
resolve-twice exposure) — and two genuine new-capability gaps, logged
here rather than patched ad hoc:

- **`deny_paths` has no way to protect a project-local secret file (e.g.
  `.env`) that recurs across arbitrary project directories** — matching is
  `starts_with` on fixed, absolute/home-relative paths only
  (`crates/aivyx-sandbox/src/lib.rs`'s `path_is_denied`), so a user would
  have to hand-add every project's own absolute `.env` path one at a time.
  `deny_paths` is documented as the *sole* protection against the model
  reading plaintext credentials via a normal, auto-allowed
  `ActionKind::Read` call — a fresh clone's `.env` is invisible to it by
  default. Needs its own design pass: basename matching, glob patterns, or
  gitignore-style relative patterns are all plausible, each with different
  false-positive/complexity tradeoffs worth walking through with the user
  rather than picking unilaterally.
- **`delegate_task` sub-agents share the parent's single global REPL
  session slot**, breaking the "fresh, isolated agent, completely separate
  conversation history" invariant `delegate_task` is documented to
  provide. `crates/aivyx/src/agent_builder.rs` clones the same
  `ToolRegistry` (same underlying `Arc<Mutex<Option<ReplSession>>>`) for
  sub-agents; `sub_agent_registry_never_contains_delegate_task_itself` is
  the only sub-agent tool exclusion that exists today. A sub-agent's
  `repl_start` call is invisible to the parent's own history, yet the
  process it starts outlives the sub-agent and can collide with (or be
  silently reused/blocked by) the parent's own REPL usage. Needs its own
  design pass: exclude REPL tools from the sub-agent registry entirely,
  give each sub-agent a private session slot, or auto-stop any session a
  sub-agent leaves running when it completes.
```

Replace it with (note the intro sentence changes from "two" to "one",
since the `deny_paths` bullet is now closed, and the surviving bullet is
renumbered from a list to a standalone sentence since it's the only one
left):

```markdown
A fresh audit (2026-07-28) covering the same four dimensions against the
current codebase found two documentation-accuracy gaps, fixed directly
(the autonomous-mode paragraph was missing `repl_start`/MCP-tool/
`remember_preference` denials added since it was written; the TOCTOU
known-limitation bullet was missing `patch_file`, which has the identical
resolve-twice exposure), and one genuine new-capability gap, logged here
rather than patched ad hoc: **`delegate_task` sub-agents share the
parent's single global REPL session slot**, breaking the "fresh, isolated
agent, completely separate conversation history" invariant `delegate_task`
is documented to provide. `crates/aivyx/src/agent_builder.rs` clones the
same `ToolRegistry` (same underlying `Arc<Mutex<Option<ReplSession>>>`)
for sub-agents; `sub_agent_registry_never_contains_delegate_task_itself`
is the only sub-agent tool exclusion that exists today. A sub-agent's
`repl_start` call is invisible to the parent's own history, yet the
process it starts outlives the sub-agent and can collide with (or be
silently reused/blocked by) the parent's own REPL usage. Needs its own
design pass: exclude REPL tools from the sub-agent registry entirely,
give each sub-agent a private session slot, or auto-stop any session a
sub-agent leaves running when it completes. (The `deny_paths` gap
previously logged alongside this one shipped — see "`deny_paths`
basename-glob matching" above.)
```

and add a new "shipped" paragraph to the "## Current status" section,
directly after the "**Verification test-selection — shipped.**"
paragraph and before the "See `docs/HISTORY.md` for the full
phase-by-phase narrative..." line:

```markdown
**`deny_paths` basename-glob matching — shipped.** The first item in the
2026-07-28 capability audit's backlog: `deny_paths` matching was
`starts_with` on fixed, absolute/home-relative paths only, so a
project-local secret file like `.env` — which recurs across arbitrary
project directories the agent might be pointed at — had no default
protection; a user would have had to hand-add every project's own
absolute path one at a time. A `deny_paths` entry with no path separator
(e.g. `.env`, `*.pem`) is now a basename-glob pattern instead, matching
any file with that name anywhere via the `globset` crate. No new config
schema — the existing `deny_paths: Vec<String>` list accepts both shapes,
distinguished by whether the entry contains a `/`. The default list gained
six new basename-glob entries (`.env`, `.env.*`, `id_rsa`, `id_ed25519`,
`*.pem`, `*.key`) alongside the existing absolute-path defaults, closing
the exact gap the audit found out of the box. A pre-existing duplicate of
the matching logic in `aivyx-tools` (used by `grep`/`glob`/`move_file`/
`git_commit`'s own directory-walk checks, and flagged by the same audit as
a drift risk) was consolidated onto the one canonical
`aivyx_sandbox::path_is_denied` function as part of this work.
```

- [ ] **Step 4: Add a HISTORY.md chapter**

In `docs/HISTORY.md`, append a new chapter after the "### 2026-07-28
capability audit — done" chapter:

```markdown
### `deny_paths` basename-glob matching — ✅ shipped

The first item in the 2026-07-28 capability audit's backlog: `deny_paths`
matching (`aivyx_sandbox::path_is_denied`) was `starts_with` over fixed,
absolute/home-relative paths only, with no way to protect a project-local
secret file — e.g. `.env` — that recurs across arbitrary project
directories the agent might be pointed at. Design spec at
`docs/superpowers/specs/2026-07-28-deny-paths-basename-glob-design.md`,
implementation plan at
`docs/superpowers/plans/2026-07-28-deny-paths-basename-glob.md`, executed
via `subagent-driven-development`.

**What shipped**: a `deny_paths` entry with no path separator (`.env`,
`*.pem`, `id_rsa`) is now a basename-glob pattern, matched via the
`globset` crate against a path's file name wherever it appears, rather
than a fixed absolute location. No new config schema — the existing
`deny_paths: Vec<String>` list infers which shape an entry is from
whether it contains a `/` (and whether it starts with `~`, which always
routes through the existing tilde-expansion path even for the bare `"~"`
case). `path_is_denied`'s public signature never changed — every existing
caller (`ConfirmationGate::check`, `LandlockConfiner`'s grant
construction, the autonomous-mode worktree-boundary check) needed zero
changes, since classification happens by inspecting the entry itself
(a single-component `PathBuf` is a basename-glob pattern; anything else
keeps the original `starts_with` check).

**A correctness fix needed in `aivyx-config`, caught during design rather
than left as a live bug**: `resolve_tilde_paths` ran every `deny_paths`
entry through symlink-canonicalization, including bare ones. For a
nonexistent bare pattern this already happened to no-op (canonicalize
fails, the walk-up logic bottoms out, the original string returns
unchanged) — but this was an accident of that function's "canonicalize
what exists, keep the rest literal" behavior, not a guarantee. If a file
literally named `.env` happened to exist wherever the process was
launched from — not necessarily the project directory being worked on —
canonicalize would have silently rewritten the pattern into an absolute
path tied to that incidental location, defeating the "matches anywhere"
semantic the feature exists to provide. Fixed by classifying each entry
*before* attempting resolution: a bare entry (no `/`, no leading `~`)
skips tilde-expansion and symlink-canonicalization entirely.

**A second, pre-existing bug closed as a side effect of this work, not a
separately-scoped task**: `aivyx-tools::path_resolve::is_denied` was a
byte-for-byte duplicate of `aivyx_sandbox::path_is_denied` — its own doc
comment said "Mirrors `ConfirmationGate::is_denied`'s exact `starts_with`
logic" — used by `grep`/`glob`/`move_file`/`git_commit` for their own
per-entry directory-walk checks. Since `aivyx-tools` already depends on
`aivyx-sandbox`, the duplicate was deleted and its four call sites
switched to calling the canonical function directly, closing the exact
drift risk the duplication represented (a future change to the matching
logic — like this one — landing in one copy and not the other) as a
natural side effect of touching this code.

**Default list gained six new basename-glob entries** (`.env`, `.env.*`,
`id_rsa`, `id_ed25519`, `*.pem`, `*.key`) alongside the existing
absolute-path defaults, closing the exact gap the audit found out of the
box rather than shipping the mechanism only and requiring users to opt
in — consistent with this project's existing conservative-security-default
posture.
```

- [ ] **Step 5: Commit**

```bash
git add README.md ROADMAP.md docs/HISTORY.md
git commit -m "Document deny_paths basename-glob matching; close the backlog item"
```
