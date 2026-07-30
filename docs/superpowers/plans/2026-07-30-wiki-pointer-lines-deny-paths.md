# `wiki_pointer_lines` `deny_paths` Enforcement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `aivyx-repomap`'s `wiki_pointer_lines` the same `deny_paths`
enforcement `collect_tags` already has — a denied wiki page's path and
summary must not reach the system prompt.

**Architecture:** One additional `.filter()` step in `wiki_pointer_lines`'s
existing iterator chain, reusing the already-correct `is_denied` function
verbatim. No new logic, no new dependency.

**Tech Stack:** Rust, no new dependencies.

## Global Constraints

- `is_denied` itself (`crates/aivyx-repomap/src/lib.rs`) must not change
  — it already correctly handles both absolute/tilde-prefixed entries and
  basename-glob patterns.
- `wiki_pointer_lines` keeps reading via plain `std::fs::read_dir` (not
  `ignore::WalkBuilder`) — this fix does not change how the directory is
  walked, only whether a denied entry is filtered out afterward.

---

### Task 1: Add the missing `deny_paths` filter to `wiki_pointer_lines`

**Files:**
- Modify: `crates/aivyx-repomap/src/lib.rs`

**Interfaces:**
- Consumes: the existing private `is_denied(path: &Path, deny_paths:
  &[PathBuf]) -> bool` function (unchanged).

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `crates/aivyx-repomap/src/lib.rs` (after
the existing `a_bare_basename_pattern_excludes_a_matching_file_from_the_map`
test):

```rust
    #[test]
    fn a_denied_wiki_page_is_excluded_from_the_pointer_lines() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "visible.rs", "pub fn visible_fn() {}\n");
        write(dir.path(), "docs/wiki/public.md", "# Public\nSome notes.\n");
        write(dir.path(), "docs/wiki/secret.md", "# Secret\nHidden notes.\n");

        let deny = vec![PathBuf::from("secret.md")];
        let map = RepoMap::new(dir.path().to_path_buf(), deny);
        let rendered = map.render(10_000).unwrap();

        assert!(rendered.contains("public.md"));
        assert!(
            !rendered.contains("secret.md"),
            "denied wiki page leaked:\n{rendered}"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p aivyx-repomap --test-threads=1 a_denied_wiki_page_is_excluded_from_the_pointer_lines`
Expected: FAIL — `secret.md` appears in the rendered output, since
`wiki_pointer_lines` has no `deny_paths` check yet.

- [ ] **Step 3: Add the missing filter**

In `crates/aivyx-repomap/src/lib.rs`, replace:

```rust
        let mut pages: Vec<(String, Option<String>)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
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

with:

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

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-repomap --test-threads=1`
Expected: all tests pass, including the new one and the pre-existing
`denied_and_gitignored_files_stay_out_of_the_map` and
`a_bare_basename_pattern_excludes_a_matching_file_from_the_map` (both
unaffected — this fix only touches `wiki_pointer_lines`, not
`collect_tags`).

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-repomap/src/lib.rs
git commit -m "Add deny_paths enforcement to wiki_pointer_lines"
```

---

### Task 2: Documentation and backlog closure

**Files:**
- Modify: `ROADMAP.md`
- Modify: `docs/HISTORY.md`

**Interfaces:**
- Consumes: nothing (documentation only); should be the last task since
  it describes the finished fix.

- [ ] **Step 1: Update ROADMAP.md**

In `ROADMAP.md`, delete this entire paragraph (in the "## Backlog"
section — it is currently the last content in the file):

```markdown
**Found at the Landlock + `aivyx-repomap` basename-glob enforcement
feature's own final whole-branch review (2026-07-30), logged rather than
expanding that feature's scope mid-review**: `aivyx-repomap`'s
`wiki_pointer_lines` (`crates/aivyx-repomap/src/lib.rs`) reads
`docs/wiki/*.md` files and injects their path + one-line summary into the
system prompt every turn, with **no `deny_paths` check at all** — unlike
`collect_tags`, which the Landlock + `aivyx-repomap` feature just added
basename-glob matching to. A user who denied a pattern matching a wiki
page (e.g. `secret*.md`) would still have its summary reach the prompt.
Low severity (scoped to `.md` files under `docs/wiki/`, and no default
`deny_paths` entry targets `.md`), but it's the one remaining "content
reaches the prompt without any deny check" path in this crate.
```

After deletion, the "## Backlog" section's last paragraph is "No new
capability opportunities were found in test quality or the
security/gate-tier-order/Landlock dimension this pass — see
`docs/HISTORY.md`'s '2026-07-28 capability audit' chapter for the full
account of what was checked." — leave that paragraph as the new end of
the file.

Then, in the "## Current status" section, find this existing text (the
end of the Docker Model Runner entry):

```markdown
against a real running instance yet — see `docs/HISTORY.md` for the full
account of what's confirmed vs. still open.

See `docs/HISTORY.md` for the full phase-by-phase narrative behind
every item above.

## Backlog — capability opportunities, not yet scheduled
```

Insert a new "shipped" paragraph between them:

```markdown
against a real running instance yet — see `docs/HISTORY.md` for the full
account of what's confirmed vs. still open.

**`wiki_pointer_lines` `deny_paths` enforcement — shipped.** Found at the
Landlock + `aivyx-repomap` basename-glob enforcement feature's own final
review: `aivyx-repomap`'s `wiki_pointer_lines` read `docs/wiki/*.md`
files and injected each page's path and summary into the system prompt
every turn with no `deny_paths` check at all — unlike `collect_tags`,
which that same feature had just given basename-glob-aware matching to.
Fixed with a one-line addition to `wiki_pointer_lines`'s existing filter
chain, reusing the same `is_denied` function verbatim. No new logic, no
new dependency.

See `docs/HISTORY.md` for the full phase-by-phase narrative behind
every item above.

## Backlog — capability opportunities, not yet scheduled
```

Also update the `_Last updated:_` line at the top of `ROADMAP.md` to
today's actual date, if it is not already today's date.

- [ ] **Step 2: Add a HISTORY.md chapter**

In `docs/HISTORY.md`, append a new chapter after the "### Docker Model
Runner serving support — documented, live verification pending" chapter
(the last chapter in the file):

```markdown
### `wiki_pointer_lines` `deny_paths` enforcement — ✅ shipped

Found at the Landlock + `aivyx-repomap` basename-glob enforcement
feature's own final whole-branch review (2026-07-30) and logged to
`ROADMAP.md`'s backlog rather than fixed mid-review, since it was out of
that feature's stated scope: `aivyx-repomap`'s `wiki_pointer_lines`
(`crates/aivyx-repomap/src/lib.rs`) read `docs/wiki/*.md` files and
injected each page's path plus a one-line summary into the system prompt
every turn, with no `deny_paths` check at all — unlike `collect_tags`,
which the same feature had just given basename-glob-aware `deny_paths`
matching via `is_denied`. A user who denied a pattern matching a wiki
page (e.g. `secret*.md`) would still have that page's path and summary
reach the model's system prompt. Low severity (no default `deny_paths`
entry targets `.md` files), but it was the one remaining "content
reaches the prompt without any deny check" path in this crate.

**The fix**: one additional `.filter()` step in `wiki_pointer_lines`'s
existing iterator chain, reusing the exact same `is_denied` function
`collect_tags` already calls — no new function, no new dependency, no
change to `is_denied` itself. `wiki_pointer_lines` still reads via plain
`std::fs::read_dir` (not `ignore::WalkBuilder`, unlike `collect_tags`'s
recursive repo walk) — a denied wiki page still needs to physically
exist in `docs/wiki/` to be excluded, same as before; only whether it's
now filtered out afterward changes.

With this shipped, the entire 2026-07-28 capability-audit backlog
lineage remains fully closed — this was a small, separately-logged
finding from a later feature's own review, not a reopened item.
```

- [ ] **Step 3: Commit**

```bash
git add ROADMAP.md docs/HISTORY.md
git commit -m "Document wiki_pointer_lines deny_paths enforcement"
```
