# Landlock + `aivyx-repomap` Basename-Glob Enforcement — Design

**Status:** Approved by user 2026-07-29.

## Context

The last open backlog item, found at the `deny_paths` basename-glob
feature's own final whole-branch review (2026-07-29) and deliberately
deferred rather than expanding that feature's scope mid-review:
basename-glob `deny_paths` entries (`.env`, `*.pem`, etc.) are enforced
for the model's own file/search/git tools via
`aivyx_sandbox::path_is_denied`, but not yet for two other surfaces:

1. **Landlock command-tool grants** (`grant_paths_excluding` in
   `crates/aivyx-sandbox/src/confiner.rs`) only understand fixed-path
   carve-outs (`starts_with`/exact-equality against an absolute root), so
   a bare pattern like `.env` never matches an absolute grant root and is
   silently never carved out — a confined `run_shell`/`run_command` child
   can still read (or, via a network-capable command, exfiltrate) a file
   matching one, even though `README.md`'s security-model section
   previously implied kernel-level enforcement applied uniformly.
2. **`aivyx-repomap`'s own duplicate `is_denied`** (a third copy never
   accounted for during the `deny_paths` feature's design) has no
   basename-glob awareness either — a repo-map-parsed source file (`.rs`/
   `.py`/`.js`/`.jsx`/`.ts`/`.tsx`) matching a user's own bare
   `deny_paths` pattern would still have its symbols/signatures reach the
   system prompt.

**Correcting a factual error in the original backlog entry**: it claimed
`aivyx-repomap` "can't add `globset`" because the crate is "deliberately
zero-dependency." Checking `crates/aivyx-repomap/Cargo.toml` shows this
crate already depends on several external crates (`ignore`,
`streaming-iterator`, `tree-sitter` and three per-language grammars) — it
has zero dependencies on *other workspace crates* specifically (no
`aivyx-sandbox`, `aivyx-tools`, etc.), which is the real, deliberate
architectural boundary (keeps repo-map extraction a pure,
independently-testable string-in/string-out component, untangled from
the security/tools layer). Adding `globset` — an external crate, not a
workspace crate, already used at the same version elsewhere in the
workspace — does not violate this boundary at all.

## Decisions

### Landlock: pre-resolve bare-pattern matches into concrete paths, reuse `grant_paths_excluding` unchanged

`grant_paths_excluding`'s existing recursive carve-out algorithm is
well-tested and stays completely untouched. Instead, a new step runs
*before* it, only for **project-relevant grant roots** — `cwd` (both
read and write sides) and each entry in `extra_read_paths` (read side
only) — converting any bare `deny_paths` pattern into the concrete
absolute paths it actually matches within that specific root, then
merging those into a root-scoped deny list before calling the existing
function:

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
        if bare_patterns.iter().any(|pattern| crate::is_basename_glob_match(&path, pattern)) {
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

`LandlockConfiner::new` calls this once per project-relevant root (`cwd`
computed once and reused for both its read and write grant, each
`extra_read_paths` entry computed separately), extends that root's own
`deny_paths` copy with the concrete matches found, and passes the
extended list into the existing `grant_paths_excluding` — which needs no
changes at all, since concrete absolute paths are exactly what it
already understands. The fixed system paths (`DEFAULT_READ_PATHS`,
`DEFAULT_HOME_READ_PATHS`) and the OS temp directory / `TMPDIR` continue
calling `grant_paths_excluding` with the plain, unmodified `deny_paths`
list — bare patterns already have zero effect on that function's
existing absolute-only checks (`.env" == "/usr"` and
`".env".starts_with("/usr")` are both trivially false), so this is a
correctness no-op for those roots, not a special case needing its own
logic.

**Why scope it this way, not to every grant root**: extending the
recursive scan to `/usr`/`/lib`/`/etc` would mean walking hundreds of
thousands of files at every session startup for a scenario that doesn't
happen — nobody's project secrets live there. `cwd` and
`extra_read_paths` are exactly the roots a project's own `.env`/`*.pem`
could plausibly live under; the fixed system paths and the shared OS
temp directory are not, and scanning them would be pure waste. `LandlockConfiner::new`
is constructed exactly once per session (confirmed by reading
`agent_builder.rs`'s single call site, wrapped in `Arc<dyn ExecutionConfiner>`
and reused for every subsequent command spawn via `confine()`) — so this
is a bounded, one-time-per-session cost proportional to project size,
the same cost category `aivyx-repomap`'s own one-time project walk
already accepts.

### New shared predicate: `is_bare_pattern`

`crates/aivyx-sandbox/src/lib.rs`'s `path_is_denied` currently inlines
its single-component classification check
(`denied.parent() == Some(Path::new(""))`) directly in its match closure.
Extracted into a small, private, reusable function:

```rust
fn is_bare_pattern(path: &Path) -> bool {
    path.parent() == Some(Path::new(""))
}
```

`path_is_denied` calls it in place of the inline check; `confiner.rs`'s
new `find_basename_glob_matches` calls the same function (private items
at the crate root are visible to descendant modules like `confiner` in
Rust, so no visibility change is needed — `confiner` already implicitly
has access). This avoids the two call sites independently re-deriving
the same classification predicate and risking disagreement — exactly the
kind of duplication class a task reviewer already flagged as a
theoretical risk during the original `deny_paths` feature (the
trailing-slash edge case discussion).

### `aivyx-repomap`: mirror the canonical matcher, add `globset`

`crates/aivyx-repomap/Cargo.toml` gains `globset = "0.4.18"` (same
version already used elsewhere in the workspace). Its own `is_denied`
gains the identical classification + basename-glob matching
`aivyx_sandbox::path_is_denied` already has — a **justified** duplicate
this time, since the real, deliberate boundary (no dependency on
`aivyx-sandbox` or any other workspace crate) stays intact; this is
categorically different from the `aivyx-tools::path_resolve::is_denied`
duplicate consolidated during the `deny_paths` feature, which existed
only because nobody had gotten around to calling the already-available
canonical function:

```rust
fn is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool {
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

The existing doc comment's incorrect "can't add `globset`" claim is
replaced with an accurate explanation of the real boundary (see
"Correcting a factual error" above) and a note that this basename-glob
logic is now duplicated here for that reason, not out of oversight.

## Out of scope for this spec

- Any change to `grant_paths_excluding` itself — it stays exactly as
  written, receiving pre-resolved concrete paths only.
- Extending the basename-glob scan to `DEFAULT_READ_PATHS`,
  `DEFAULT_HOME_READ_PATHS`, or the OS temp directory / `TMPDIR` — a
  deliberate, documented scope limit (see "Why scope it this way" above),
  not an oversight.
- Solving the inherent TOCTOU-shaped gap where a file matching a bare
  pattern, created *after* `LandlockConfiner::new` runs inside an
  already-broadly-granted directory, isn't retroactively excluded.
  Landlock rulesets are static once built; closing this would mean
  rebuilding the ruleset (and re-walking the project) on every command
  spawn, reintroducing the exact per-command performance cost the
  once-per-session design avoids, for a narrow race window. Documented
  as an accepted limitation, alongside this project's other already-accepted
  TOCTOU classes (symlink-swap timing on `write_file`/`edit_file`/
  `read_file`/`patch_file`).
- Consolidating `aivyx-repomap`'s `is_denied` onto the canonical
  `aivyx_sandbox::path_is_denied` — structurally impossible without
  breaking the crate's zero-workspace-dependency boundary, which is out
  of scope to change here.

## Testing / verification

`crates/aivyx-sandbox/src/confiner.rs`:
- `find_basename_glob_matches` (unit-level, no real Landlock/root
  privileges needed): finds a bare-pattern match nested at depth
  (proving recursion works, not just top-level entries); returns an
  empty `Vec` immediately when `deny_paths` has no bare entries (the
  fast-path case); does not follow a symlinked directory into a cycle.
- One integration-level test, mirroring the existing
  `deny_paths_entry_nested_inside_cwd_is_excluded_from_the_grant` test's
  shape: a real confined `cat` of a file matching a bare pattern nested
  inside `cwd` fails, while a sibling non-matching file remains readable.
- No test asserting system paths are *not* scanned — the code structure
  itself (a fully separate candidate-handling path for
  `DEFAULT_READ_PATHS`/`DEFAULT_HOME_READ_PATHS`/temp dirs, calling
  `grant_paths_excluding` directly with the plain `deny_paths`) makes this
  self-evident to a reviewer reading the diff; a test proving "no
  filesystem walk happened" would be awkward to write meaningfully.

`crates/aivyx-repomap/src/lib.rs`:
- Extend the existing `denied_and_gitignored_files_stay_out_of_the_map`-style
  test (or add a sibling test following its exact shape) proving a bare
  pattern (e.g. a repomap-parsed `.rs` file matching `secret*.rs`) is
  excluded from the rendered map, while a non-matching sibling `.rs` file
  still appears.
- A test proving the existing absolute/tilde-prefixed-entry behavior is
  completely unchanged (the pre-existing `denied_and_gitignored_files_stay_out_of_the_map`
  test itself continues passing unmodified).

**Live E2E verification** (manual follow-up after implementation, matching
how every other feature in this project has been verified — see
`docs/HISTORY.md`): confirm on the bare-metal rig that a real confined
`run_shell` command can no longer `cat` a project `.env` file, and that
a real model session's repo map genuinely omits a source file matching a
user-configured bare pattern.

## Documentation

`README.md`'s "1. `deny_paths` — a hard block" section (already updated
by the `deny_paths` feature to note the Landlock gap) gets that caveat
corrected: instead of "not yet enforced at this layer," it should now
describe the actual, narrower scope (`cwd` and `extra_read_paths` are
covered; the fixed system paths and OS temp directory are not, by
deliberate design). `ROADMAP.md`'s backlog entry for this item is
removed and replaced with a "shipped" paragraph in Current status,
matching every other closed backlog item's treatment — this closes the
very last open item from the entire 2026-07-28 audit lineage.
`docs/HISTORY.md` gets a narrative chapter, matching the existing
chapters' depth, including the factual correction about `aivyx-repomap`'s
actual dependency boundary.
