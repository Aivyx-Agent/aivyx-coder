# `deny_paths` Basename-Glob Matching — Design

**Status:** Approved by user 2026-07-28.

## Context

The first item in the 2026-07-28 capability audit's backlog (tracked in
`ROADMAP.md`): `deny_paths` matching is `starts_with` over fixed,
absolute/home-relative paths only (`aivyx_sandbox::path_is_denied`,
`crates/aivyx-sandbox/src/lib.rs`). There is no way to protect a
project-local secret file — e.g. `.env` — that recurs across arbitrary
project directories the agent might be pointed at; a user would have to
hand-add every project's own absolute `.env` path one at a time.
`README.md` documents `deny_paths` as the *sole* protection against the
model reading plaintext credentials via a normal, auto-allowed
`ActionKind::Read` call — a fresh clone's `.env` is invisible to it by
default today.

Two design questions were resolved with the user via one-at-a-time
questions before this doc was written:

1. **Matching mechanism**: basename glob. An entry with no path separator
   (e.g. `.env` or `*.pem`) matches any file with that name/pattern
   anywhere under the resolved path, via the `globset` crate already used
   by the `glob` tool — not basename-exact-only (misses variable-name
   secrets like `id_rsa` vs. `id_ed25519`, `*.pem` vs. `*.key`) and not
   full gitignore-style patterns (too much unneeded complexity — `**`,
   negation, directory-only anchors — for what's fundamentally a simple
   credential-blocklist need).
2. **Default list**: yes, add a small set of new basename-glob entries
   closing the exact gap the audit found, rather than shipping the
   mechanism only and leaving users to opt in. Matches this project's
   existing conservative-security-default posture.

## Decisions

### No new config schema — classification is inferred from the entry itself

A `deny_paths` entry (`Vec<String>` in `aivyx-config`, unchanged) is
classified by whether it contains a path separator (`/`):

- **Contains a separator** (`~/.ssh`, `/etc/foo`, `~/.docker/config.json`)
  → today's exact behavior: tilde-expanded, symlink-canonicalized, then
  matched via `path.starts_with(denied)`. Zero behavior change.
- **No separator** (`.env`, `*.pem`, `id_rsa`) → a new basename-glob
  entry: compiled via `globset::Glob::new(&raw)?.compile_matcher()` and
  matched against `path.file_name()` only. Never tilde-expanded, never
  symlink-canonicalized (see below for why).

No new TOML field, no new type in the public config surface — the
existing `deny_paths: Vec<String>` list simply accepts both shapes,
distinguished by the presence of `/`.

### `path_is_denied`: same signature, new per-entry branch

`aivyx_sandbox::path_is_denied(path: &Path, deny_paths: &[PathBuf]) ->
bool` (`crates/aivyx-sandbox/src/lib.rs`) keeps its exact signature —
every one of its current callers (`ConfirmationGate::check`,
`LandlockConfiner`'s grant construction, `agent/mod.rs`'s autonomous
worktree-boundary check) needs zero changes. Only the function body
changes: for each `denied` entry, if it has no parent component (i.e. it
is itself a bare `PathBuf` with a single component — equivalently,
`denied.parent() == Some(Path::new(""))` or `denied == Path::new(&denied.file_name().unwrap_or_default())`),
treat it as a basename-glob pattern and test `path.file_name()` against
the compiled matcher; otherwise keep today's `path.starts_with(denied)`.

`aivyx-sandbox` gains `globset` as a new dependency (already at version
`0.4.18` elsewhere in the workspace via `aivyx-tools`; same version here).

Glob compilation happens per-call rather than being precomputed and
cached: `deny_paths` lists are small (order of 15-20 entries even after
this feature's additions), `Glob::new(...).compile_matcher()` is a cheap,
allocation-light operation, and precomputing/caching would add a new
struct threaded through every one of `path_is_denied`'s existing call
sites for a cost that isn't measurable in practice — consistent with this
project's YAGNI principle. If a future profiling pass finds this is
actually hot (e.g. inside `move_file`'s large-directory recursive scan),
precomputing a cached `GlobSet` becomes a pure internal optimization with
no signature or behavior change, not a redesign.

### `aivyx-config`: classify before resolving, not after

`resolve_tilde_paths` (`crates/aivyx-config/src/lib.rs`) currently runs
*every* `deny_paths` entry through tilde-expansion and
`resolve_symlinks`'s canonicalization. For a bare entry with no existing
file matching it at config-load time, this already happens to be a
no-op today (canonicalize fails, the walk-up logic bottoms out at an
empty parent, and the original string is returned unchanged) — but this
is an accident of the current implementation's "canonicalize what
exists, keep the rest literal" behavior, not a guarantee. If a file
*literally* named `.env` or `*.pem` (an unlikely but not impossible
literal filename) happened to exist in whatever directory the process
was launched from — not necessarily the project directory being worked
on — canonicalize would silently rewrite the pattern into an absolute
path tied to that incidental location, breaking the "matches anywhere"
semantic the whole feature exists to provide.

Fix: `resolve_tilde_paths` classifies each raw string *before* attempting
any resolution. An entry with no `/` skips tilde-expansion and
`resolve_symlinks` entirely, passing through as the literal raw string
(wrapped in a `PathBuf` for the return type, unchanged from today's
`Vec<PathBuf>` signature). An entry with a `/` proceeds through exactly
today's resolution path, byte-for-byte unchanged.

This is a strict improvement over today's behavior for such entries, not
a breaking change in practice: a bare, non-tilde, non-absolute entry
today already has fragile, launch-directory-dependent semantics (whatever
`Path::canonicalize` happens to do relative to the process's actual
current directory at config-load time) — there is no existing default or
documented use case relying on that fragile behavior, since every current
default entry is tilde-prefixed.

### Consolidating the duplicate matcher (closes a drift risk found during design)

`aivyx-tools::path_resolve::is_denied` (`crates/aivyx-tools/src/path_resolve.rs`)
is a byte-for-byte duplicate of `aivyx_sandbox::path_is_denied` — its own
doc comment states it "Mirrors `ConfirmationGate::is_denied`'s exact
`starts_with` logic." It exists because `grep`/`glob`/`move_file`/
`git_commit` need to check individual paths encountered during their own
directory walks, not just their top-level target, against `deny_paths`.

Since `aivyx-tools` already depends on `aivyx-sandbox` (used elsewhere
for `ActionKind`/`PermissionRequest`/`PermissionTarget`), this duplicate
function is deleted, and its four call sites
(`crates/aivyx-tools/src/tools/{grep,glob,move_file,git_commit}.rs`,
currently `use crate::path_resolve::{is_denied, resolve}`) switch their
import to `aivyx_sandbox::path_is_denied` directly, keeping `resolve`
from `path_resolve` unchanged. This closes the exact drift risk the
duplication represents — a future change to the matching logic (like
this feature) landing in one copy and not the other — as a natural side
effect of touching this code, not a separately-scoped task.

### Default list additions

New basename-glob entries in `PermissionSettings::default()`'s
`deny_paths` (`crates/aivyx-config/src/lib.rs`), alongside the existing
absolute-path defaults:

```rust
deny_paths: vec![
    // ... existing absolute-path entries, unchanged ...
    ".env".to_string(),
    ".env.*".to_string(),
    "id_rsa".to_string(),
    "id_ed25519".to_string(),
    "*.pem".to_string(),
    "*.key".to_string(),
],
```

`.env.*` (glob) catches `.env.local`/`.env.production`/etc. alongside the
exact `.env` entry. `id_rsa`/`id_ed25519` are OpenSSH's two current
default private-key filenames (no glob needed — these are fixed
conventional names, unlike the certificate/key file extensions below).
`*.pem`/`*.key` catch arbitrary-named certificate/key files by extension.

## Out of scope for this spec

- Any new TOML schema, field, or config-file syntax — this is a matching
  behavior change to the existing `deny_paths: Vec<String>`, not a new
  config surface.
- Directory-component glob patterns (e.g. `secrets/*.pem`) — only bare,
  separator-free basename patterns are supported; anything with a `/`
  keeps today's prefix-match semantics exactly.
- Precomputed/cached `GlobSet` construction — deferred as a pure internal
  optimization if ever needed (see the `path_is_denied` decision above).
- Case-insensitive matching — `globset`'s default case-sensitive matching
  is correct for this project's Linux-only sandbox target.

## Testing / verification

`aivyx-sandbox` (`crates/aivyx-sandbox/src/lib.rs`):
- A bare pattern (`.env`) denies a path with that basename under an
  arbitrary directory, and under a *different* arbitrary directory too
  (proving "matches anywhere," not a fixed location).
- A bare glob pattern (`*.pem`) denies any basename matching the glob;
  a non-matching basename (`.env`) under the same directory is not
  denied by that same entry.
- A path-separator entry (`~/.ssh`, already resolved to an absolute path
  by the caller) keeps exact `starts_with` behavior — a sibling file
  outside `~/.ssh` is not denied, a file inside it is.
- A bare pattern does not accidentally match a directory or file whose
  full path merely *contains* the pattern text as a substring outside
  the basename position (e.g. a directory named `.env-backup` is not
  denied by a bare `.env` entry, since glob matching is exact against
  the whole basename, not a substring search).

`aivyx-config` (`crates/aivyx-config/src/lib.rs`):
- A bare entry (`.env`) in `resolved_deny_paths()` passes through
  unchanged regardless of what exists on disk at the process's actual
  launch directory — proven by creating a real file named `.env` in a
  temp dir, setting it as the current directory for the test, and
  confirming the resolved entry is still the literal string `.env`, not
  an absolute path into that temp dir.
- Existing tilde-prefixed and non-tilde absolute-path tests
  (`tilde_prefixed_deny_paths_expand_to_the_home_directory`,
  `non_tilde_deny_paths_pass_through_unchanged`) continue to pass
  unchanged.
- `PermissionSettings::default()`'s `deny_paths` includes the six new
  entries alongside all previously-asserted absolute-path entries
  (extend the existing `default_deny_paths_covers_common_credential_locations`
  test rather than adding a parallel one).

`aivyx-tools` (`crates/aivyx-tools/src/tools/move_file.rs`, and spot
checks in `grep.rs`/`glob.rs`/`git_commit.rs`):
- `move_file`'s existing `a_gitignored_deny_path_nested_in_the_directory_is_still_caught`
  test (and its sibling non-gitignored case) continue to pass after the
  import switch from `path_resolve::is_denied` to
  `aivyx_sandbox::path_is_denied`, proving the consolidation is a pure
  refactor with no behavior change for the prefix-match case.
- One new test per tool proving a bare basename-glob `deny_paths` entry
  (not just a prefix-path one) is caught during that tool's own
  directory-walk check — e.g. `grep`/`glob` searching a directory
  containing a nested `.env` refuses or skips it; `git_commit` refuses to
  stage a nested `.env`.

**Live E2E verification** (manual follow-up after implementation, matching
how every other tool/config change in this project has been verified —
see `docs/HISTORY.md`): confirm a real model asked to read a `.env` file
in a fresh test project directory (not one of the pre-configured
`~/`-anchored defaults) is refused via the normal deny_paths message, and
that a legitimate, non-matching file in the same directory is still
readable normally.

## Documentation

`README.md`'s `deny_paths` config-reference section gets a new paragraph
explaining basename-glob entries (an entry with no `/` matches by
filename anywhere, an entry with a `/` keeps today's exact prefix
semantics), the updated default list, and a short worked example (`.env`,
`*.pem`) alongside the existing absolute-path examples. `ROADMAP.md`'s
backlog entry for this item is removed and replaced with a "shipped"
paragraph in Current status, matching every other closed backlog item's
treatment; `docs/HISTORY.md` gets a narrative chapter, matching the
existing chapters' depth.
