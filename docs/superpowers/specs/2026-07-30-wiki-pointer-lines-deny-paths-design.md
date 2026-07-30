# `wiki_pointer_lines` `deny_paths` Enforcement — Design

**Status:** Approved by user 2026-07-30.

## Context

Found at the Landlock + `aivyx-repomap` basename-glob enforcement
feature's own final whole-branch review (2026-07-30) and logged to
`ROADMAP.md`'s backlog rather than fixed mid-review, since it was out of
that feature's stated scope: `aivyx-repomap`'s `wiki_pointer_lines`
(`crates/aivyx-repomap/src/lib.rs:135`) reads `docs/wiki/*.md` files and
injects each page's path plus a one-line summary into the system prompt
every turn, with **no `deny_paths` check at all** — unlike `collect_tags`
(`crates/aivyx-repomap/src/lib.rs:168`), which the same feature just gave
basename-glob-aware `deny_paths` matching via `is_denied`
(`crates/aivyx-repomap/src/lib.rs:240`).

Concretely: a user who denies a pattern matching a wiki page (e.g.
`secret*.md`, or an absolute path to a specific page) would still have
that page's path and summary reach the model's system prompt via
`wiki_pointer_lines`, even though the identical pattern is now correctly
excluded from `collect_tags`'s own repo-map output. Low severity — no
*default* `deny_paths` entry targets `.md` files, so this has zero
impact out of the box — but it's the one remaining "content reaches the
prompt without any deny check" path in this crate.

## Decisions

### Reuse `is_denied` directly — no new logic

`wiki_pointer_lines` gains one additional filter step in its existing
iterator chain, using the exact same `is_denied(path, deny_paths)`
function `collect_tags` already calls (both are private functions in the
same file; no visibility or module changes needed):

```rust
let mut pages: Vec<(String, Option<String>)> = entries
    .filter_map(|e| e.ok())
    .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
    .filter(|e| !is_denied(&e.path(), &self.deny_paths))
    .map(|e| {
        let path = e.path();
        let relative = path.strip_prefix(&self.root).unwrap_or(&path).to_path_buf();
        let summary = std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| wiki_summary(&content));
        (relative.display().to_string(), summary)
    })
    .collect();
```

This is a one-line addition to an existing filter chain — no new
function, no new dependency, no change to `is_denied` itself (which
already supports both absolute/tilde-prefixed entries and basename-glob
patterns, unchanged since the prior feature).

### `wiki_pointer_lines` reads via plain `std::fs::read_dir`, not `ignore::WalkBuilder` — deliberately unchanged

Unlike `collect_tags` (which walks the whole repo recursively via
`ignore::WalkBuilder`), `wiki_pointer_lines` only reads the immediate
contents of one fixed directory (`docs/wiki/`, non-recursive) via
`std::fs::read_dir`. This fix doesn't change that — a denied wiki page
still needs to physically exist in that directory to be excluded, same
as before; only whether it's now *filtered out* changes. No walk-strategy
change is needed or in scope here.

## Out of scope for this spec

- Any change to `collect_tags` or `is_denied` themselves — both are
  already correct as of the prior feature.
- Making `wiki_pointer_lines` recursive, or changing what counts as a
  "wiki page" — purely adding the missing deny check to the existing
  logic.

## Testing / verification

Unit test added to `crates/aivyx-repomap/src/lib.rs`'s existing test
module, mirroring `a_bare_basename_pattern_excludes_a_matching_file_from_the_map`'s
shape (the test added for `collect_tags`'s own basename-glob fix):

- A `docs/wiki/` directory with two `.md` pages, one matching a bare
  `deny_paths` pattern and one not, plus at least one real `.rs` file
  with a definition (`render()` returns `None` if every collected file's
  `tags.defs` is empty, regardless of wiki content, so the test needs a
  genuine source file to reach the wiki-lines code path at all — this is
  existing `render()` behavior, not something this fix changes).
  Confirms the non-denied page's path appears in `render()`'s output and
  the denied page's does not.

No live E2E follow-up needed beyond this project's normal bar — this is
a straightforward, fully-unit-testable fix with no external service
dependency, unlike the Docker Model Runner or Landlock-adjacent work
this session.

## Documentation

`ROADMAP.md`'s backlog paragraph for this item is removed and replaced
with a "shipped" paragraph in Current status, matching every other
closed backlog item's treatment. `docs/HISTORY.md` gets a short
narrative chapter, matching the existing chapters' depth (this one can
be brief, given the fix's small size).
