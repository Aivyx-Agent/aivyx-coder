# Phase 11b — Agent-Maintained Codebase Wiki Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `/wiki`, a slash command that generates and keeps a fixed set of Markdown pages under `docs/wiki/` up to date with the codebase, driven entirely through the agent's existing gated tools and turn loop — no new trust tier.

**Architecture:** A deterministic staleness/frontmatter module in `aivyx-tools` (git diff against each page's recorded commit + covered paths, no LLM involved) feeds a per-page turn-orchestration module in `aivyx-core` (`Agent::run_wiki_turn`, intercepted in `Agent::run_turn` next to `/council`, driving one turn per stale page through the existing `TurnPaused` continuation mechanism). `aivyx-repomap` gains a lightweight pointer-list section so the model can discover wiki pages without a new token budget.

**Tech Stack:** Rust, tokio, existing `aivyx-tools`/`aivyx-core`/`aivyx-repomap` crates. No new external dependencies — frontmatter parsing is hand-rolled (no YAML crate exists anywhere in this workspace today; the format is simple and fully self-controlled).

## Global Constraints

- No existing interactive-mode, plan-mode, autonomous-mode, or council-mode behavior may change. `/wiki` adds a new command-interception branch; it must not alter what happens for any input that isn't `/wiki`/`/wiki <page>`.
- Every `write_file` call `/wiki` causes goes through the existing `ConfirmationGate`/confirmation-modal/checkpoint path unchanged — no batching, no pre-approval, no new gate tier. This is explicitly **not** a security-critical change like Phase 11c.
- Run `cargo test --workspace` and `cargo clippy --workspace --all-targets` after every task. Commit after every task.
- No new dependencies added to any crate's `Cargo.toml` without a documented reason — frontmatter parsing must be hand-rolled, matching this workspace's existing precedent (`edit_blocks.rs` hand-rolls SEARCH/REPLACE parsing rather than pulling in a parser crate).
- `aivyx-repomap` stays "deliberately dependency-free of the rest of the workspace" (its own module doc comment, Phase 6) — it must **not** gain a dependency on `aivyx-tools` or `aivyx-core`. Its wiki-summary extraction is a small, independently hand-rolled helper, duplicated rather than shared; this is intentional and must not be "fixed" into a shared dependency by a later task or reviewer.
- Frontmatter format (fixed by this plan, not literally YAML-spec-compliant, hand-parsed):
  ```
  ---
  generated_at_commit: <full 40-char SHA>
  covers:
    - "path/one"
    - "path/two"
  summary: "One-line description."
  ---
  <body>
  ```
  `covers` entries are plain path prefixes (files or directories, e.g. `"crates/aivyx-core"`), **not** glob patterns — git's `diff -- <pathspec>` already matches everything under a directory prefix by default, so no `/**` suffix or `:(glob)` magic is needed. This refines the design spec's illustrative `"crates/aivyx-core/**"` example; the spec's intent (a page's covered paths) is unchanged, only the concrete pathspec syntax, verified against real git behavior rather than assumed — the same discipline Phase 11c applied to its own git plumbing.
- `architecture-overview.md`'s `covers` list is fixed at implementation time in code (Task 3, `ARCHITECTURE_OVERVIEW_COVERS`), not re-curated by the model on every run. The design spec describes it as "curated by the agent itself at generation time"; this plan implements that curation once, as a constant, rather than inventing a mechanism for the model to durably communicate a re-curated list back to the deterministic staleness layer on every invocation. `generated_at_commit` must always be stamped by code, never trusted from model output (this part is explicit in the spec); extending the same "code owns bookkeeping" principle to `covers` for this one page keeps staleness fully deterministic and avoids a fragile model-authored-frontmatter-parsing path for a field whose correctness matters for correctness (not just cosmetics). This is a disclosed, deliberate scope decision — flag it in review, don't silently "fix" it back to a per-run agent-curated list without a follow-up design conversation.
- `Agent::run_wiki_turn` reuses `run_turn_inner` directly (not `run_turn`, to avoid re-checking a synthesized instruction against `/council`/`/wiki` prefixes). Every synthesized instruction it sends becomes a real `Role::User` history entry, exactly like a normal turn or Phase 11c's synthesized "continue" messages — this is existing, accepted precedent, not a new gap.
- `write_file` calls made during a `/wiki` batch flow through the *same* `unverified_edits`/enforced-verification logic already built into `run_turn_inner` (Phase 12 Part B) with zero special-casing — if `[verification].command` is configured, it will auto-run after a wiki page's edit exactly as it would for a code edit. This is an intentional, inherited consequence of reusing `run_turn_inner` unmodified, not an oversight: document it, do not try to suppress verification for wiki-only batches.

---

### Task 1: `aivyx-tools::wiki` — types and frontmatter parsing

**Files:**
- Create: `crates/aivyx-tools/src/wiki.rs`
- Modify: `crates/aivyx-tools/src/lib.rs` (register the module, no new pub re-export yet — Task 2 adds the pub surface)

**Interfaces:**
- Produces: `pub struct PageSpec { pub name: String, pub covers: Vec<String> }`, `pub enum StaleReason { Missing, Stale, Forced }` (`Debug, Clone, Copy, PartialEq, Eq`), `pub struct StalePage { pub name: String, pub covers: Vec<String>, pub reason: StaleReason }` (`Debug, Clone, PartialEq, Eq`), `pub struct Frontmatter { pub generated_at_commit: Option<String>, pub covers: Vec<String>, pub summary: Option<String> }` (`Debug, Clone, Default, PartialEq, Eq`), `pub fn parse_frontmatter(content: &str) -> (Frontmatter, &str)`, `pub fn page_path(wiki_dir: &Path, name: &str) -> PathBuf`.

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-tools/src/wiki.rs`:

```rust
//! Agent-maintained codebase wiki (ROADMAP.md Phase 11b): deterministic
//! staleness detection and frontmatter bookkeeping for the pages under
//! `docs/wiki/`. No LLM judgment lives here — this module only ever answers
//! "does this page's recorded commit + covered paths look out of date" and
//! "rewrite this page's bookkeeping fields," both via plain git plumbing and
//! hand-rolled text parsing. `aivyx-core::wiki` owns everything about *which*
//! pages exist and how turns are driven to fill them in.

use std::path::{Path, PathBuf};

/// One page this project's wiki could have: its name (file stem under
/// `docs/wiki/`) and the path prefixes its content is staked against for
/// staleness purposes. Built by `aivyx-core::wiki::page_specs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSpec {
    pub name: String,
    pub covers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    /// The page file doesn't exist yet under `docs/wiki/`.
    Missing,
    /// The page exists but its covered paths changed since
    /// `generated_at_commit` (or its frontmatter couldn't be trusted).
    Stale,
    /// Forced via `/wiki <page>`, regardless of actual staleness.
    Forced,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalePage {
    pub name: String,
    pub covers: Vec<String>,
    pub reason: StaleReason,
}

/// A page's `---`-delimited frontmatter block, hand-parsed (no YAML crate
/// exists anywhere in this workspace, and the format `stamp_page` writes is
/// simple and fully self-controlled). Fields absent from the block, or a
/// missing block entirely, come back as `None`/empty rather than erroring —
/// callers decide what an absent field means (see `stale_pages`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub generated_at_commit: Option<String>,
    pub covers: Vec<String>,
    pub summary: Option<String>,
}

/// Splits `content` into its frontmatter (if any) and the body after it.
/// `content` with no leading `---\n` block returns default frontmatter and
/// the whole input as the body — this is the expected shape for a
/// freshly-model-written page that skipped the frontmatter template, not an
/// error case.
pub fn parse_frontmatter(content: &str) -> (Frontmatter, &str) {
    let mut fm = Frontmatter::default();
    let Some(after_open) = content.strip_prefix("---\n") else {
        return (fm, content);
    };
    let Some(close_at) = after_open.find("\n---\n") else {
        // The block runs to the end of the file with no trailing body.
        if let Some(block) = after_open.strip_suffix("\n---\n") {
            parse_frontmatter_lines(block, &mut fm);
            return (fm, "");
        }
        // No closing marker at all — treat the whole thing as an
        // unrecognized body rather than guessing.
        return (fm, content);
    };
    let (block, rest) = after_open.split_at(close_at);
    parse_frontmatter_lines(block, &mut fm);
    let body = rest.strip_prefix("\n---\n").unwrap_or(rest);
    (fm, body.trim_start_matches('\n'))
}

fn parse_frontmatter_lines(block: &str, fm: &mut Frontmatter) {
    let mut lines = block.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(value) = line.strip_prefix("generated_at_commit: ") {
            fm.generated_at_commit = Some(value.trim().to_string());
        } else if line.trim() == "covers:" {
            while let Some(next) = lines.peek() {
                let Some(item) = next.trim().strip_prefix("- ") else {
                    break;
                };
                fm.covers.push(unquote(item));
                lines.next();
            }
        } else if let Some(value) = line.strip_prefix("summary: ") {
            fm.summary = Some(unquote(value.trim()));
        }
    }
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

/// The path a page named `name` lives at under `wiki_dir`.
pub fn page_path(wiki_dir: &Path, name: &str) -> PathBuf {
    wiki_dir.join(format!("{name}.md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_extracts_all_known_fields() {
        let content = "---\n\
             generated_at_commit: abc123\n\
             covers:\n\
             \x20\x20- \"crates/aivyx-core\"\n\
             \x20\x20- \"Cargo.toml\"\n\
             summary: \"Turn loop and orchestration.\"\n\
             ---\n\
             \n\
             # aivyx-core\n\
             \n\
             Body text.\n";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.generated_at_commit.as_deref(), Some("abc123"));
        assert_eq!(fm.covers, vec!["crates/aivyx-core", "Cargo.toml"]);
        assert_eq!(fm.summary.as_deref(), Some("Turn loop and orchestration."));
        assert_eq!(body, "# aivyx-core\n\nBody text.\n");
    }

    #[test]
    fn parse_frontmatter_returns_empty_when_no_frontmatter_present() {
        let content = "# Just a page\n\nNo frontmatter here.\n";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm, Frontmatter::default());
        assert_eq!(body, content);
    }

    #[test]
    fn parse_frontmatter_handles_a_block_with_no_covers_or_summary() {
        let content = "---\ngenerated_at_commit: deadbeef\n---\nBody only.\n";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.generated_at_commit.as_deref(), Some("deadbeef"));
        assert!(fm.covers.is_empty());
        assert_eq!(fm.summary, None);
        assert_eq!(body, "Body only.\n");
    }

    #[test]
    fn page_path_joins_the_wiki_dir_and_name_with_md_extension() {
        let wiki_dir = Path::new("/repo/docs/wiki");
        assert_eq!(
            page_path(wiki_dir, "aivyx-core"),
            PathBuf::from("/repo/docs/wiki/aivyx-core.md")
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they compile and pass**

Run: `cargo test -p aivyx-tools wiki:: -- --nocapture`
Expected: 4 tests pass (the module doesn't exist as a build target yet until Step 3 registers it — this step's real purpose is confirming the module compiles standalone; if `cargo test -p aivyx-tools` fails with "unresolved module `wiki`", that's expected until Step 3).

- [ ] **Step 3: Register the module**

In `crates/aivyx-tools/src/lib.rs`, add near the other `mod` declarations:

```rust
mod checkpoint;
mod diff;
mod path_resolve;
mod process;
mod tools;
mod wiki;
```

(Insert `mod wiki;` alphabetically alongside the existing five — no `pub` yet; Task 2 adds the `pub use` once the git-touching functions exist and the module has a real public surface worth exporting.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tools wiki::`
Expected: `test wiki::tests::parse_frontmatter_extracts_all_known_fields ... ok` and 3 more, all passing.

- [ ] **Step 5: Run the full workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/wiki.rs crates/aivyx-tools/src/lib.rs
git commit -m "Phase 11b: aivyx-tools wiki frontmatter types and parser"
```

---

### Task 2: `aivyx-tools::wiki` — staleness detection and frontmatter stamping

**Files:**
- Modify: `crates/aivyx-tools/src/checkpoint.rs` (expose `run_git` as `pub(crate)`)
- Modify: `crates/aivyx-tools/src/wiki.rs` (add `stale_pages`, `stamp_page`)
- Modify: `crates/aivyx-tools/src/lib.rs` (add `pub mod wiki;` — the module needs a real public surface now)

**Interfaces:**
- Consumes: `Task 1`'s `PageSpec`, `StalePage`, `StaleReason`, `Frontmatter`, `parse_frontmatter`, `page_path`. The now-`pub(crate)` `checkpoint::run_git(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Result<String, String>`.
- Produces: `pub async fn stale_pages(cwd: &Path, wiki_dir: &Path, specs: &[PageSpec], cancellation: &CancellationToken) -> Vec<StalePage>`, `pub async fn stamp_page(wiki_dir: &Path, cwd: &Path, name: &str, covers: &[String], cancellation: &CancellationToken) -> Result<(), String>`. `Task 4` (Agent orchestration) calls both directly.

- [ ] **Step 1: Expose `run_git` to the rest of the crate**

In `crates/aivyx-tools/src/checkpoint.rs`, change the function signature (currently private, used only within this file):

```rust
async fn run_git(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Result<String, String> {
```

to:

```rust
pub(crate) async fn run_git(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Result<String, String> {
```

No other change to this file — every existing call site already resolves to the same function.

- [ ] **Step 2: Write the implementation and its tests together**

Unlike Task 1's pure string-parsing (where tests could meaningfully precede an empty stub), `stale_pages`/`stamp_page` need real git plumbing to exercise at all — there's no useful intermediate "fails to compile" state worth a separate step here. Add the following as one unit: the production code (`git`, `stale_pages`, `render_frontmatter`, `DEFAULT_SUMMARY`, `stamp_page`) as new top-level items in `crates/aivyx-tools/src/wiki.rs`, **after** the existing `#[cfg(test)] mod tests` block from Task 1, followed by a brand-new, separate `#[cfg(test)] mod git_tests { ... }` block (kept separate from Task 1's `mod tests` so the real-git fixture tests are easy to run in isolation via `cargo test -p aivyx-tools wiki::git_tests`). The two new `use` lines (`tokio_util::sync::CancellationToken` and `crate::checkpoint::run_git`) go at the very top of the file, alongside the existing `use std::path::{Path, PathBuf};`:

```rust
use tokio_util::sync::CancellationToken;

use crate::checkpoint::run_git;

/// Wraps `run_git` with cooperative cancellation, matching
/// `GitCheckpointer::git`'s own wrapper — this module talks to git via the
/// same shared plumbing, just without a private index (staleness/stamping
/// never touch the worktree or any index, only read commits and rewrite one
/// file).
async fn git(cwd: &Path, args: &[&str], cancellation: &CancellationToken) -> Result<String, String> {
    tokio::select! {
        result = run_git(cwd, args, &[]) => result,
        _ = cancellation.cancelled() => Err("cancelled".to_string()),
    }
}

/// For each `spec`, determines whether its page is missing or stale against
/// HEAD. Never errors: any git failure (unreachable `generated_at_commit`,
/// no repository, etc.) degrades to `StaleReason::Stale` rather than
/// blocking `/wiki` outright — matches `ToolExecutor`'s own checkpoint
/// wrappers' "degrade to a safe default" precedent.
pub async fn stale_pages(
    cwd: &Path,
    wiki_dir: &Path,
    specs: &[PageSpec],
    cancellation: &CancellationToken,
) -> Vec<StalePage> {
    let mut out = Vec::new();
    for spec in specs {
        if cancellation.is_cancelled() {
            break;
        }
        let path = page_path(wiki_dir, &spec.name);
        let Ok(content) = std::fs::read_to_string(&path) else {
            out.push(StalePage {
                name: spec.name.clone(),
                covers: spec.covers.clone(),
                reason: StaleReason::Missing,
            });
            continue;
        };
        let (fm, _) = parse_frontmatter(&content);
        let Some(commit) = fm.generated_at_commit else {
            out.push(StalePage {
                name: spec.name.clone(),
                covers: spec.covers.clone(),
                reason: StaleReason::Stale,
            });
            continue;
        };

        let mut args: Vec<&str> = vec!["diff", "--name-only", &commit, "HEAD", "--"];
        args.extend(spec.covers.iter().map(String::as_str));
        let is_stale = match git(cwd, &args, cancellation).await {
            Ok(diff) => !diff.trim().is_empty(),
            Err(_) => true, // unreachable commit, or any other git failure
        };
        if is_stale {
            out.push(StalePage {
                name: spec.name.clone(),
                covers: spec.covers.clone(),
                reason: StaleReason::Stale,
            });
        }
    }
    out
}

const DEFAULT_SUMMARY: &str = "See page for details.";

fn render_frontmatter(generated_at_commit: &str, covers: &[String], summary: &str) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("generated_at_commit: {generated_at_commit}\n"));
    out.push_str("covers:\n");
    for c in covers {
        out.push_str(&format!("  - \"{c}\"\n"));
    }
    out.push_str(&format!("summary: \"{summary}\"\n"));
    out.push_str("---\n");
    out
}

/// Rewrites `name`'s page frontmatter to the authoritative state: the real
/// current HEAD commit and `covers` as passed in — never trusted from
/// whatever the model wrote, if anything (see this plan's Global
/// Constraints). Preserves a `summary` if the model's own draft included
/// one and it's non-empty; otherwise falls back to `DEFAULT_SUMMARY`. Errors
/// if the page file doesn't exist (the model never called `write_file`) —
/// callers treat that as "this page stays stale, retried next `/wiki` run,"
/// not a batch-aborting failure.
pub async fn stamp_page(
    wiki_dir: &Path,
    cwd: &Path,
    name: &str,
    covers: &[String],
    cancellation: &CancellationToken,
) -> Result<(), String> {
    let path = page_path(wiki_dir, name);
    let content = std::fs::read_to_string(&path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    let (fm, body) = parse_frontmatter(&content);
    let summary = fm
        .summary
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SUMMARY.to_string());

    let head = git(cwd, &["rev-parse", "HEAD"], cancellation)
        .await?
        .trim()
        .to_string();

    let mut out = render_frontmatter(&head, covers, &summary);
    out.push('\n');
    out.push_str(body);
    std::fs::write(&path, out).map_err(|err| format!("could not write {}: {err}", path.display()))
}

#[cfg(test)]
mod git_tests {
    use super::*;

    async fn init_repo(dir: &Path) {
        for argv in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "test"],
            vec!["config", "user.email", "test@test.invalid"],
        ] {
            run_git(dir, &argv, &[]).await.unwrap();
        }
        std::fs::write(dir.join("tracked.txt"), "v1\n").unwrap();
        run_git(dir, &["add", "-A"], &[]).await.unwrap();
        run_git(dir, &["commit", "-q", "-m", "initial"], &[])
            .await
            .unwrap();
    }

    async fn commit_all(dir: &Path, message: &str) -> String {
        run_git(dir, &["add", "-A"], &[]).await.unwrap();
        run_git(dir, &["commit", "-q", "-m", message], &[])
            .await
            .unwrap();
        run_git(dir, &["rev-parse", "HEAD"], &[])
            .await
            .unwrap()
            .trim()
            .to_string()
    }

    fn spec(name: &str, covers: &[&str]) -> PageSpec {
        PageSpec {
            name: name.to_string(),
            covers: covers.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[tokio::test]
    async fn stale_pages_reports_missing_for_a_page_that_does_not_exist_yet() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let wiki_dir = dir.path().join("docs/wiki");

        let result = stale_pages(
            dir.path(),
            &wiki_dir,
            &[spec("aivyx-core", &["crates/aivyx-core"])],
            &CancellationToken::new(),
        )
        .await;

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason, StaleReason::Missing);
    }

    #[tokio::test]
    async fn stale_pages_reports_stale_when_covered_files_changed_since_generation() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        std::fs::create_dir_all(dir.path().join("crates/aivyx-core")).unwrap();
        std::fs::write(dir.path().join("crates/aivyx-core/lib.rs"), "// v1\n").unwrap();
        let generated_at = commit_all(dir.path(), "add crate file").await;

        let wiki_dir = dir.path().join("docs/wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        std::fs::write(
            page_path(&wiki_dir, "aivyx-core"),
            format!("---\ngenerated_at_commit: {generated_at}\n---\nold content\n"),
        )
        .unwrap();

        // A change under the covered path after the page was generated.
        std::fs::write(dir.path().join("crates/aivyx-core/lib.rs"), "// v2\n").unwrap();
        commit_all(dir.path(), "change crate file").await;

        let result = stale_pages(
            dir.path(),
            &wiki_dir,
            &[spec("aivyx-core", &["crates/aivyx-core"])],
            &CancellationToken::new(),
        )
        .await;

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason, StaleReason::Stale);
    }

    #[tokio::test]
    async fn stale_pages_omits_a_page_whose_covered_files_are_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        std::fs::create_dir_all(dir.path().join("crates/aivyx-core")).unwrap();
        std::fs::write(dir.path().join("crates/aivyx-core/lib.rs"), "// v1\n").unwrap();
        let generated_at = commit_all(dir.path(), "add crate file").await;

        let wiki_dir = dir.path().join("docs/wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        std::fs::write(
            page_path(&wiki_dir, "aivyx-core"),
            format!("---\ngenerated_at_commit: {generated_at}\n---\ncurrent\n"),
        )
        .unwrap();

        // A commit that touches something NOT under the covered path.
        std::fs::write(dir.path().join("unrelated.txt"), "noise\n").unwrap();
        commit_all(dir.path(), "unrelated change").await;

        let result = stale_pages(
            dir.path(),
            &wiki_dir,
            &[spec("aivyx-core", &["crates/aivyx-core"])],
            &CancellationToken::new(),
        )
        .await;

        assert!(result.is_empty(), "unrelated changes must not mark the page stale");
    }

    #[tokio::test]
    async fn stale_pages_treats_malformed_frontmatter_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let wiki_dir = dir.path().join("docs/wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        std::fs::write(page_path(&wiki_dir, "aivyx-core"), "no frontmatter at all\n").unwrap();

        let result = stale_pages(
            dir.path(),
            &wiki_dir,
            &[spec("aivyx-core", &["crates/aivyx-core"])],
            &CancellationToken::new(),
        )
        .await;

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason, StaleReason::Stale);
    }

    #[tokio::test]
    async fn stale_pages_treats_unreachable_generated_at_commit_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let wiki_dir = dir.path().join("docs/wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        std::fs::write(
            page_path(&wiki_dir, "aivyx-core"),
            "---\ngenerated_at_commit: 0000000000000000000000000000000000000000\n---\nbody\n",
        )
        .unwrap();

        let result = stale_pages(
            dir.path(),
            &wiki_dir,
            &[spec("aivyx-core", &["crates/aivyx-core"])],
            &CancellationToken::new(),
        )
        .await;

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason, StaleReason::Stale);
    }

    #[tokio::test]
    async fn stamp_page_preserves_model_written_summary_and_overwrites_bookkeeping_fields() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let wiki_dir = dir.path().join("docs/wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        std::fs::write(
            page_path(&wiki_dir, "aivyx-core"),
            "---\n\
             generated_at_commit: placeholder-the-model-made-up\n\
             covers:\n  - \"nonsense\"\n\
             summary: \"Turn loop and orchestration.\"\n\
             ---\n\
             # aivyx-core\n\nReal body.\n",
        )
        .unwrap();
        let real_head = run_git(dir.path(), &["rev-parse", "HEAD"], &[])
            .await
            .unwrap()
            .trim()
            .to_string();

        stamp_page(
            &wiki_dir,
            dir.path(),
            "aivyx-core",
            &["crates/aivyx-core".to_string()],
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        let written = std::fs::read_to_string(page_path(&wiki_dir, "aivyx-core")).unwrap();
        let (fm, body) = parse_frontmatter(&written);
        assert_eq!(fm.generated_at_commit.as_deref(), Some(real_head.as_str()));
        assert_eq!(fm.covers, vec!["crates/aivyx-core"]);
        assert_eq!(fm.summary.as_deref(), Some("Turn loop and orchestration."));
        assert_eq!(body, "# aivyx-core\n\nReal body.\n");
    }

    #[tokio::test]
    async fn stamp_page_falls_back_to_a_default_summary_when_the_model_wrote_none() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let wiki_dir = dir.path().join("docs/wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        std::fs::write(page_path(&wiki_dir, "aivyx-core"), "just a body, no frontmatter\n").unwrap();

        stamp_page(
            &wiki_dir,
            dir.path(),
            "aivyx-core",
            &["crates/aivyx-core".to_string()],
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        let written = std::fs::read_to_string(page_path(&wiki_dir, "aivyx-core")).unwrap();
        let (fm, body) = parse_frontmatter(&written);
        assert_eq!(fm.summary.as_deref(), Some(DEFAULT_SUMMARY));
        assert_eq!(body, "just a body, no frontmatter\n");
    }

    #[tokio::test]
    async fn stamp_page_errors_when_the_page_file_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let wiki_dir = dir.path().join("docs/wiki");

        let result = stamp_page(
            &wiki_dir,
            dir.path(),
            "never-written",
            &[],
            &CancellationToken::new(),
        )
        .await;

        assert!(result.is_err());
    }
}
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tools wiki::`
Expected: all `wiki::tests::*` (4, from Task 1) and `wiki::git_tests::*` (7, new) tests pass — 11 total.

- [ ] **Step 4: Export the module**

In `crates/aivyx-tools/src/lib.rs`, change:

```rust
mod wiki;
```

to:

```rust
pub mod wiki;
```

(Every other internal module — `checkpoint`, `diff`, `path_resolve`, `process`, `tools` — stays private; `wiki` is the first to need a direct public surface beyond what's re-exported at the crate root, since `aivyx-core` needs `wiki::PageSpec`/`StalePage`/`StaleReason`/`stale_pages`/`stamp_page` by their full paths, matching how `aivyx_tools::GitCheckpointer` etc. are already consumed by name elsewhere.)

- [ ] **Step 5: Run the full workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/checkpoint.rs crates/aivyx-tools/src/wiki.rs crates/aivyx-tools/src/lib.rs
git commit -m "Phase 11b: aivyx-tools wiki staleness detection and frontmatter stamping"
```

---

### Task 3: `aivyx-core::wiki` — command parsing and page skeleton

**Files:**
- Create: `crates/aivyx-core/src/wiki.rs`
- Modify: `crates/aivyx-core/src/lib.rs` (register the module)

**Interfaces:**
- Consumes: `aivyx_tools::wiki::PageSpec` (Task 1/2).
- Produces: `pub enum WikiCommand { Batch, Forced(String) }`, `pub fn parse_command(input: &str) -> Option<WikiCommand>`, `pub const WIKI_DIR: &str = "docs/wiki"`, `pub fn page_specs(cwd: &Path) -> Vec<PageSpec>`. `Task 4` (`Agent::run_wiki_turn`) consumes all four.

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-core/src/wiki.rs`:

```rust
//! Agent-maintained codebase wiki (ROADMAP.md Phase 11b): the `/wiki`
//! command and this project's fixed page skeleton. Mirrors `council.rs`'s
//! shape (command parsing + orchestration living beside `Agent`), not its
//! content — wiki page generation needs the full read/search/write tool
//! loop `Agent::run_turn_inner` already drives, not a tool-free
//! `stream_chat` call. Deterministic staleness/frontmatter mechanics live in
//! `aivyx_tools::wiki`; this module owns *which* pages exist for this
//! project and how turns get driven to fill them in (`Agent::run_wiki_turn`,
//! in `agent.rs`).

use std::path::Path;

use aivyx_tools::wiki::PageSpec;

/// Where generated wiki pages live, relative to the process `cwd`.
pub const WIKI_DIR: &str = "docs/wiki";

/// `architecture-overview.md`'s curated coverage: cross-cutting structural
/// files, not the whole repo — deliberately fixed here rather than
/// re-curated by the model on every run (see this plan's Global
/// Constraints). Update this list by hand if the workspace's structural
/// shape changes materially.
const ARCHITECTURE_OVERVIEW_COVERS: &[&str] = &[
    "Cargo.toml",
    "crates/aivyx/src/main.rs",
    "crates/aivyx-core/src/agent.rs",
    "crates/aivyx-tui/src/app.rs",
];

/// Recognized `/wiki` forms. Bare `/wiki` regenerates whatever
/// `aivyx_tools::wiki::stale_pages` reports; `/wiki <page>` forces exactly
/// that one page regardless of its staleness state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikiCommand {
    Batch,
    Forced(String),
}

/// Recognizes `/wiki` / `/wiki <page>` (and nothing else — a message merely
/// starting with those letters is a normal turn), mirroring
/// `council::parse_command` exactly.
pub fn parse_command(input: &str) -> Option<WikiCommand> {
    let trimmed = input.trim();
    let rest = trimmed.strip_prefix("/wiki")?;
    if rest.is_empty() {
        Some(WikiCommand::Batch)
    } else if rest.starts_with(char::is_whitespace) {
        let page = rest.trim();
        if page.is_empty() {
            Some(WikiCommand::Batch)
        } else {
            Some(WikiCommand::Forced(page.to_string()))
        }
    } else {
        None
    }
}

/// This project's full page skeleton: `architecture-overview` plus one page
/// per workspace crate, discovered from `cwd/crates/*` (a directory
/// containing a `Cargo.toml`) rather than hand-maintained, so a new crate
/// automatically gets a page without this list needing an update. Crate
/// directory names in this workspace are identical to their package names
/// (verified: `crates/aivyx-core` contains package `aivyx-core`), so the
/// directory name is used directly without parsing `Cargo.toml`.
pub fn page_specs(cwd: &Path) -> Vec<PageSpec> {
    let mut specs = vec![PageSpec {
        name: "architecture-overview".to_string(),
        covers: ARCHITECTURE_OVERVIEW_COVERS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }];

    let mut crate_names: Vec<String> = std::fs::read_dir(cwd.join("crates"))
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().join("Cargo.toml").is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    crate_names.sort();

    for name in crate_names {
        specs.push(PageSpec {
            covers: vec![format!("crates/{name}")],
            name,
        });
    }
    specs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_command_recognizes_bare_and_forced_forms() {
        assert_eq!(parse_command("/wiki"), Some(WikiCommand::Batch));
        assert_eq!(parse_command("  /wiki  "), Some(WikiCommand::Batch));
        assert_eq!(
            parse_command("/wiki aivyx-core"),
            Some(WikiCommand::Forced("aivyx-core".to_string()))
        );
    }

    #[test]
    fn parse_command_rejects_lookalikes_and_normal_messages() {
        assert_eq!(parse_command("/wikifoo"), None);
        assert_eq!(parse_command("run /wiki for me"), None);
        assert_eq!(parse_command("wiki"), None);
    }

    #[test]
    fn page_specs_includes_architecture_overview_and_discovered_crates_sorted() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["zeta-crate", "alpha-crate"] {
            let crate_dir = dir.path().join("crates").join(name);
            std::fs::create_dir_all(&crate_dir).unwrap();
            std::fs::write(crate_dir.join("Cargo.toml"), "[package]\n").unwrap();
        }
        // A directory without a Cargo.toml must not become a page.
        std::fs::create_dir_all(dir.path().join("crates/not-a-crate")).unwrap();

        let specs = page_specs(dir.path());
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["architecture-overview", "alpha-crate", "zeta-crate"]
        );
        let alpha = specs.iter().find(|s| s.name == "alpha-crate").unwrap();
        assert_eq!(alpha.covers, vec!["crates/alpha-crate"]);
    }

    #[test]
    fn page_specs_handles_a_missing_crates_directory_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let specs = page_specs(dir.path());
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "architecture-overview");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-core wiki::`
Expected: FAIL — `crates::aivyx_core::wiki` isn't a registered module yet (`error[E0433]: failed to resolve: use of unresolved module or unlinked crate `wiki``, or the file simply isn't compiled).

- [ ] **Step 3: Register the module**

In `crates/aivyx-core/src/lib.rs`, add `pub mod wiki;` alongside the existing modules:

```rust
pub mod agent;
pub mod council;
pub mod edit_blocks;
pub mod session;
pub mod wiki;

pub use agent::{Agent, AgentConfig, AgentError, AgentEvent, EditFormat};
pub use council::{Council, CouncilSeat};
pub use session::{SessionState, Task, TaskStatus};
```

(No new top-level re-export — `wiki::WikiCommand`/`wiki::parse_command`/`wiki::page_specs`/`wiki::WIKI_DIR` are consumed by their full path from `agent.rs`, matching how `council::parse_command` already is.)

Add `tempfile = "3.27.0"` to `crates/aivyx-core/Cargo.toml`'s `[dev-dependencies]` if it isn't already present — check first:

Run: `grep tempfile crates/aivyx-core/Cargo.toml`

If that prints nothing, add it:

```toml
[dev-dependencies]
tempfile = "3.27.0"
```

(matching the version already used by `aivyx-tools` and `aivyx-repomap`).

Also add `aivyx-tools` and `aivyx-repomap` — check first, they're almost certainly already dependencies of `aivyx-core` (the crate already imports `aivyx_tools::ToolExecutor` and `aivyx_repomap::RepoMap` per `agent.rs`'s existing `use` lines) — this step should be a no-op; only add if genuinely missing:

Run: `grep -E "^aivyx-(tools|repomap)" crates/aivyx-core/Cargo.toml`

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-core wiki::`
Expected: 4 tests pass.

- [ ] **Step 5: Run the full workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-core/src/wiki.rs crates/aivyx-core/src/lib.rs crates/aivyx-core/Cargo.toml
git commit -m "Phase 11b: aivyx-core wiki command parsing and page skeleton"
```

---

### Task 4: `Agent::run_wiki_turn` — per-page orchestration and command interception

**Files:**
- Modify: `crates/aivyx-core/src/agent.rs`

**Interfaces:**
- Consumes: `crate::wiki::{WikiCommand, parse_command, page_specs, WIKI_DIR}` (Task 3); `aivyx_tools::wiki::{StalePage, StaleReason, stale_pages, stamp_page}` (Task 2); `Agent::run_turn_inner` (existing, private, same file), `Agent::last_turn_paused` field (existing), `Agent::emit` (existing).
- Produces: a new private `Agent::run_wiki_turn` method and a new match arm inside `Agent::run_turn`. No new public API — `/wiki` is reached the same way `/council` is, through `Agent::run_turn`'s existing public signature.

- [ ] **Step 1: Write the failing tests**

In `crates/aivyx-core/src/agent.rs`'s `#[cfg(test)] mod tests` block, add near `autonomous_mode_discards_and_rewinds_on_exhausted_verification` (reuse that test's `init_git_repo` helper — do not duplicate it):

```rust
    // ----- /wiki (Phase 11b) -----

    fn stale(name: &str, covers: &[&str]) -> aivyx_tools::wiki::StalePage {
        aivyx_tools::wiki::StalePage {
            name: name.to_string(),
            covers: covers.iter().map(|s| s.to_string()).collect(),
            reason: aivyx_tools::wiki::StaleReason::Missing,
        }
    }

    fn write_call(id: &str, path: &str, content: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ToolCallComplete(ToolCall {
                id: ToolCallId(id.to_string()),
                name: "write_file".to_string(),
                arguments: serde_json::json!({ "path": path, "content": content }),
                source: ToolCallSource::Native,
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
            },
        ]
    }

    #[tokio::test]
    async fn wiki_batch_regenerates_missing_pages_and_stamps_frontmatter() {
        // Uses `run_wiki_turn_for_pages` directly (an explicit page list)
        // rather than the full `run_wiki_turn` → `page_specs`/`stale_pages`
        // chain — that chain is already covered by Task 2's aivyx-tools
        // tests and by the dispatch-specific tests below; this test's job
        // is purely "given a page needs regenerating, is the orchestration
        // (one turn, then stamp) correct."
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path()).await;

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::WriteFileTool));

        let (tx, _rx) = unbounded_channel();
        let mock = Arc::new(MockBackend::new(vec![
            write_call(
                "c1",
                "docs/wiki/aivyx-core.md",
                "---\nsummary: \"Turn loop.\"\n---\n# aivyx-core\nBody.\n",
            ),
            text_response("done"), // no more tool calls: the page's turn ends
        ]));
        let llm: Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let executor = ToolExecutor::new(registry, gate, confiner);
        let mut agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig { max_tool_iterations: 10, ..Default::default() },
            Arc::default(),
            PlanMode::new(),
            AutonomousMode::new(),
            tx,
        );

        agent
            .run_wiki_turn_for_pages(
                vec![stale("aivyx-core", &["crates/aivyx-core"])],
                dir.path(),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let written = std::fs::read_to_string(dir.path().join("docs/wiki/aivyx-core.md")).unwrap();
        let (fm, body) = aivyx_tools::wiki::parse_frontmatter(&written);
        assert!(fm.generated_at_commit.is_some(), "frontmatter must be stamped");
        assert_eq!(fm.covers, vec!["crates/aivyx-core"]);
        assert_eq!(fm.summary.as_deref(), Some("Turn loop."));
        assert_eq!(body, "# aivyx-core\nBody.\n");
    }

    #[tokio::test]
    async fn wiki_bare_invocation_with_nothing_stale_emits_notice_and_writes_nothing() {
        // No `crates/` directory at all in this tempdir, so
        // `crate::wiki::page_specs` (Task 3) discovers exactly one page:
        // `architecture-overview` (matches
        // `page_specs_handles_a_missing_crates_directory_gracefully`'s
        // behavior) — stamping *that* page at the real current HEAD is what
        // makes `stale_pages` report nothing stale.
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path()).await;
        let wiki_dir = dir.path().join("docs/wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let head = tokio::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .await
            .unwrap();
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        // `covers` is deliberately omitted here: `stale_pages` only reads
        // `generated_at_commit` back from a page's own frontmatter — the
        // covered paths it diffs against come from `spec.covers`
        // (`crate::wiki::ARCHITECTURE_OVERVIEW_COVERS` for this page), not
        // from the file on disk. Since `generated_at_commit` here equals
        // the repo's current HEAD with no commits made since, `git diff
        // --name-only <head> HEAD -- <anything>` is trivially empty
        // regardless of whether those covered paths exist in this minimal
        // fixture repo.
        std::fs::write(
            wiki_dir.join("architecture-overview.md"),
            format!("---\ngenerated_at_commit: {head}\n---\nbody\n"),
        )
        .unwrap();

        let registry = ToolRegistry::new(); // no write_file registered: none must be called
        let (tx, mut rx) = unbounded_channel();
        let mock = Arc::new(MockBackend::new(vec![]));
        let llm: Arc<dyn LlmBackend> = mock;
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let executor = ToolExecutor::new(registry, gate, confiner);
        let mut agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig::default(),
            Arc::default(),
            PlanMode::new(),
            AutonomousMode::new(),
            tx,
        );

        agent
            .run_wiki_turn(crate::wiki::WikiCommand::Batch, dir.path(), CancellationToken::new())
            .await
            .unwrap();

        let events = drain(&mut rx);
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Error(m) if m.contains("up to date"))));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnComplete)));
    }

    #[tokio::test]
    async fn wiki_forced_invocation_with_unknown_page_name_rejects_with_no_turn() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path()).await;

        let registry = ToolRegistry::new();
        let (tx, mut rx) = unbounded_channel();
        let mock = Arc::new(MockBackend::new(vec![]));
        let llm: Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let executor = ToolExecutor::new(registry, gate, confiner);
        let mut agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig::default(),
            Arc::default(),
            PlanMode::new(),
            AutonomousMode::new(),
            tx,
        );

        agent
            .run_wiki_turn(
                crate::wiki::WikiCommand::Forced("not-a-real-page".to_string()),
                dir.path(),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let events = drain(&mut rx);
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Error(m) if m.contains("unknown wiki page"))));
        assert_eq!(mock.received.lock().unwrap().len(), 0, "no turn should have been sent to the backend");
    }

    #[tokio::test]
    async fn wiki_continues_to_the_next_page_after_one_page_writes_nothing() {
        // `run_wiki_turn_for_pages` takes an explicit page list, so there's
        // no dependency on `page_specs`' filesystem-driven crate discovery
        // here — no `crates/` subdirectories need to exist.
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path()).await;

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::WriteFileTool));

        let (tx, _rx) = unbounded_channel();
        // Page "alpha" (processed first, in list order) never calls
        // write_file — just prose, so its turn ends after one request.
        // Page "beta" does call write_file, which forces a *second* request
        // for that turn (the model needs a follow-up round-trip after a
        // tool call to produce the "nothing more to do" response that ends
        // the turn) — three requests total, not two.
        let mock = Arc::new(MockBackend::new(vec![
            text_response("I looked around but decided not to write anything."),
            write_call("c1", "docs/wiki/beta.md", "---\nsummary: \"Beta.\"\n---\nBeta body.\n"),
            text_response("done"),
        ]));
        let llm: Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let executor = ToolExecutor::new(registry, gate, confiner);
        let mut agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig { max_tool_iterations: 10, ..Default::default() },
            Arc::default(),
            PlanMode::new(),
            AutonomousMode::new(),
            tx,
        );

        let pages = vec![stale("alpha", &["crates/alpha"]), stale("beta", &["crates/beta"])];
        agent
            .run_wiki_turn_for_pages(pages, dir.path(), CancellationToken::new())
            .await
            .unwrap();

        assert!(
            !dir.path().join("docs/wiki/alpha.md").exists(),
            "a page the model never wrote must not appear on disk"
        );
        assert!(
            dir.path().join("docs/wiki/beta.md").exists(),
            "the next page must still be attempted after a prior page wrote nothing"
        );
        assert_eq!(
            mock.received.lock().unwrap().len(),
            3,
            "both pages must have been attempted (1 request for alpha, 2 for beta)"
        );
    }

    #[tokio::test]
    async fn run_turn_dispatches_wiki_commands_before_normal_turn_processing() {
        // Reuses the "nothing stale" fixture from
        // `wiki_bare_invocation_with_nothing_stale_emits_notice_and_writes_nothing`
        // so this test can also assert zero LLM calls happened. What's
        // distinct here: routing through the public `Agent::run_turn` entry
        // point (every real caller's entry point), not `run_wiki_turn`
        // directly — confirming the interception wiring added to
        // `run_turn`'s own dispatch `match` in this task actually fires.
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path()).await;
        let wiki_dir = dir.path().join("docs/wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let head = tokio::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .await
            .unwrap();
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        std::fs::write(
            wiki_dir.join("architecture-overview.md"),
            format!("---\ngenerated_at_commit: {head}\n---\nbody\n"),
        )
        .unwrap();

        let registry = ToolRegistry::new();
        let (mut agent, mut rx, mock) = build_agent(vec![], registry, 10);

        agent
            .run_turn("/wiki".to_string(), dir.path(), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            mock.received.lock().unwrap().len(),
            0,
            "with nothing stale, /wiki must short-circuit before any LLM call"
        );
        // `agent.history` is empty in this scenario (no turn ever ran), so
        // this is a vacuous-but-real regression guard: it would fail the
        // moment a future change pushed the raw command into history before
        // the staleness check.
        assert!(
            !agent.history.iter().any(|m| m.text_content().contains("/wiki")),
            "the raw /wiki command text must never enter LLM history"
        );
        let _ = drain(&mut rx);
    }
```

This test file references `agent.run_wiki_turn_for_pages(pages, ...)` — a second, slightly lower-level entry point taking an already-built `Vec<StalePage>` directly, so a test can exercise the "process this exact page list" behavior without depending on `stale_pages`'/`page_specs`' filesystem discovery. Design `run_wiki_turn` (the command-driven entry point) to call `run_wiki_turn_for_pages` internally after resolving `WikiCommand` into a page list — see Step 4 below.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-core wiki 2>&1 | head -40`
Expected: compile errors — `run_wiki_turn`/`run_wiki_turn_for_pages` don't exist yet.

- [ ] **Step 3: Add the `use` import**

At the top of `crates/aivyx-core/src/agent.rs`, alongside the existing `use` block:

```rust
use aivyx_tools::ToolExecutor;
```

add (this crate already depends on `aivyx_tools`, so this is just a new item from an existing dependency):

```rust
use aivyx_tools::wiki::StalePage;
```

- [ ] **Step 4: Implement `run_wiki_turn` and `run_wiki_turn_for_pages`**

Add these as new `impl Agent` methods in `crates/aivyx-core/src/agent.rs`, placed after `run_council_turn` and before `run_turn_inner`:

```rust
    /// Runs `/wiki` (Phase 11b): resolves `command` into the pages that
    /// need regenerating, then drives one turn per page through
    /// `run_wiki_turn_for_pages`. See `aivyx_tools::wiki` for the
    /// staleness/frontmatter mechanics and `crate::wiki` for this project's
    /// fixed page skeleton.
    async fn run_wiki_turn(
        &mut self,
        command: crate::wiki::WikiCommand,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<(), AgentError> {
        let wiki_dir = cwd.join(crate::wiki::WIKI_DIR);
        let specs = crate::wiki::page_specs(cwd);

        let pages: Vec<StalePage> = match command {
            crate::wiki::WikiCommand::Batch => {
                aivyx_tools::wiki::stale_pages(cwd, &wiki_dir, &specs, &cancellation).await
            }
            crate::wiki::WikiCommand::Forced(name) => {
                let Some(spec) = specs.iter().find(|s| s.name == name) else {
                    let valid: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
                    self.emit(AgentEvent::Error(format!(
                        "unknown wiki page '{name}' — valid pages: {}",
                        valid.join(", ")
                    )));
                    self.emit(AgentEvent::TurnComplete);
                    return Ok(());
                };
                vec![StalePage {
                    name: spec.name.clone(),
                    covers: spec.covers.clone(),
                    reason: aivyx_tools::wiki::StaleReason::Forced,
                }]
            }
        };

        if pages.is_empty() {
            self.emit(AgentEvent::Error(
                "wiki is up to date, nothing to regenerate".to_string(),
            ));
            self.emit(AgentEvent::TurnComplete);
            return Ok(());
        }

        self.run_wiki_turn_for_pages(pages, cwd, cancellation).await
    }

    /// Drives one turn per page in `pages`, in order — split out from
    /// `run_wiki_turn` so tests can exercise a known page list directly
    /// without depending on filesystem-driven staleness discovery. Reuses
    /// `run_turn_inner` (not `run_turn`, so a synthesized instruction is
    /// never re-checked against `/council`/`/wiki`), continuing on
    /// `TurnPaused` exactly like a normal multi-round-trip turn. A page
    /// whose turn ends in error, or whose `write_file` call never actually
    /// happened, is skipped (left stale for the next `/wiki` run) rather
    /// than aborting pages still queued behind it.
    async fn run_wiki_turn_for_pages(
        &mut self,
        pages: Vec<StalePage>,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<(), AgentError> {
        let wiki_dir = cwd.join(crate::wiki::WIKI_DIR);

        for page in pages {
            if cancellation.is_cancelled() {
                break;
            }

            let covers_list: String = page
                .covers
                .iter()
                .map(|c| format!("  - \"{c}\"\n"))
                .collect();
            let instruction = format!(
                "Regenerate the wiki page `{}/{}.md`. It should document: {}. Write clear, \
                 accurate Markdown covering what this part of the codebase does, its key \
                 types/functions, and how it's used — a reader with no context should be able \
                 to orient quickly. Start the file with frontmatter exactly in this form (fill \
                 in `summary` with a genuinely useful one-line description; \
                 `generated_at_commit` and `covers` will be overwritten automatically \
                 afterward, so their values here don't matter):\n\
                 ---\n\
                 generated_at_commit: (placeholder)\n\
                 covers:\n{covers_list}\
                 summary: \"...\"\n\
                 ---\n\
                 Then the page body. Call write_file with the complete file content in one \
                 call.",
                crate::wiki::WIKI_DIR,
                page.name,
                page.covers.join(", "),
            );

            self.last_turn_paused = false;
            let mut result = self.run_turn_inner(instruction, cwd, cancellation.clone()).await;
            while result.is_ok() && self.last_turn_paused && !cancellation.is_cancelled() {
                self.last_turn_paused = false;
                result = self
                    .run_turn_inner("continue".to_string(), cwd, cancellation.clone())
                    .await;
            }

            if cancellation.is_cancelled() {
                break;
            }
            if result.is_err() {
                // `run_turn_inner` already emitted an `AgentEvent::Error`
                // describing the failure — this page just stays stale for
                // the next `/wiki` run.
                continue;
            }

            if let Err(err) =
                aivyx_tools::wiki::stamp_page(&wiki_dir, cwd, &page.name, &page.covers, &cancellation)
                    .await
            {
                self.emit(AgentEvent::Error(format!(
                    "page `{}` did not save correctly ({err}) — it will be retried on the next \
                     /wiki run",
                    page.name
                )));
            }
        }

        self.emit(AgentEvent::TurnComplete);
        Ok(())
    }
```

- [ ] **Step 5: Wire the interception into `run_turn`**

In `crates/aivyx-core/src/agent.rs`, change `run_turn`'s dispatch `match`:

```rust
        let result = match crate::council::parse_command(&user_input) {
            Some(subject) => {
                let subject = subject.to_string();
                self.run_council_turn(&subject, cancellation).await
            }
            None => self.run_turn_inner(user_input, cwd, cancellation).await,
        };
```

to:

```rust
        let result = match crate::council::parse_command(&user_input) {
            Some(subject) => {
                let subject = subject.to_string();
                self.run_council_turn(&subject, cancellation).await
            }
            None => match crate::wiki::parse_command(&user_input) {
                Some(command) => self.run_wiki_turn(command, cwd, cancellation).await,
                None => self.run_turn_inner(user_input, cwd, cancellation).await,
            },
        };
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p aivyx-core wiki`
Expected: all 5 new tests in the `mod tests` block pass, plus the pre-existing `wiki::*` unit tests from Task 3.

- [ ] **Step 7: Run the full workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx-core/src/agent.rs
git commit -m "Phase 11b: Agent::run_wiki_turn — per-page orchestration and /wiki interception"
```

---

### Task 5: Repo-map wiki pointer section

**Files:**
- Modify: `crates/aivyx-repomap/src/lib.rs`

**Interfaces:**
- Consumes: nothing new from other crates — this task is deliberately self-contained (see Global Constraints: `aivyx-repomap` must not gain a dependency on `aivyx-tools`/`aivyx-core`).
- Produces: `RepoMap::render`'s output gains a trailing wiki-pointer section when `docs/wiki/*.md` files exist under `self.root`. No new public API.

- [ ] **Step 1: Write the failing tests**

In `crates/aivyx-repomap/src/lib.rs`'s `#[cfg(test)] mod tests` block, add (reuse the existing `write` test helper already defined there):

```rust
    #[test]
    fn render_includes_a_wiki_pointer_section_with_summaries() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src.rs", "pub fn something() {}\n");
        write(
            dir.path(),
            "docs/wiki/aivyx-core.md",
            "---\ngenerated_at_commit: abc\nsummary: \"Turn loop and orchestration.\"\n---\nBody.\n",
        );

        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        let rendered = map.render(10_000).unwrap();

        assert!(rendered.contains("docs/wiki/aivyx-core.md"));
        assert!(rendered.contains("Turn loop and orchestration."));
    }

    #[test]
    fn render_wiki_pointer_falls_back_when_a_page_has_no_summary() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src.rs", "pub fn something() {}\n");
        write(dir.path(), "docs/wiki/plain.md", "no frontmatter here\n");

        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        let rendered = map.render(10_000).unwrap();

        assert!(rendered.contains("docs/wiki/plain.md"));
    }

    #[test]
    fn render_wiki_pointer_section_is_absent_with_no_wiki_pages() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src.rs", "pub fn something() {}\n");

        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        let rendered = map.render(10_000).unwrap();

        assert!(!rendered.contains("Wiki pages"));
    }

    #[test]
    fn render_does_not_panic_with_a_tiny_budget_and_wiki_pages_present() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src.rs", "pub fn something() {}\n");
        write(
            dir.path(),
            "docs/wiki/aivyx-core.md",
            "---\nsummary: \"Should not fit in a tiny budget.\"\n---\nBody.\n",
        );

        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        // A 1-token budget (4 chars) is smaller than the header alone, so
        // `render` returns `None` here (matches existing behavior for the
        // file-listing section — not asserted as a hardcoded byte count,
        // just confirmed not to panic now that the wiki section shares the
        // same budget check). `Some(...)` would also be an acceptable
        // outcome if the budget math ever changes; the only real assertion
        // is "doesn't panic and stays within budget if it returns Some."
        if let Some(rendered) = map.render(1) {
            assert!(rendered.chars().count() < 200);
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-repomap render_includes_a_wiki`
Expected: FAIL — `rendered.contains("docs/wiki/aivyx-core.md")` is false (no wiki section exists yet).

- [ ] **Step 3: Implement the wiki pointer section**

In `crates/aivyx-repomap/src/lib.rs`, add near the top (alongside the other `const` definitions):

```rust
/// Where generated wiki pages live, relative to `root` — duplicated from
/// `aivyx-core::wiki::WIKI_DIR` rather than shared: this crate is
/// deliberately dependency-free of the rest of the workspace (see the
/// module doc comment at the top of this file). Keep both literals in sync
/// if this path ever changes.
const WIKI_DIR: &str = "docs/wiki";
```

Add this method to `impl RepoMap` (after `render`, before `collect_tags`):

```rust
    /// Lightweight pointer lines for existing wiki pages (path + one-line
    /// summary, if the page has one) — cheap enough to include every turn;
    /// the model reads a page's full content via `read_file` only if the
    /// pointer looks relevant. See ROADMAP.md Phase 11b.
    fn wiki_pointer_lines(&self) -> Vec<String> {
        let wiki_dir = self.root.join(WIKI_DIR);
        let Ok(entries) = std::fs::read_dir(&wiki_dir) else {
            return Vec::new();
        };

        let mut pages: Vec<(String, Option<String>)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .filter_map(|e| {
                let path = e.path();
                let relative = path.strip_prefix(&self.root).unwrap_or(&path).to_path_buf();
                let summary = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|content| wiki_summary(&content));
                Some((relative.display().to_string(), summary))
            })
            .collect();
        pages.sort();

        if pages.is_empty() {
            return Vec::new();
        }
        let mut lines = vec!["\nWiki pages (read via read_file for full detail):\n".to_string()];
        lines.extend(pages.into_iter().map(|(path, summary)| match summary {
            Some(s) => format!("  {path}: {s}\n"),
            None => format!("  {path}\n"),
        }));
        lines
    }
```

In `render`, insert the new section after the existing per-file loop but before the final return. Change:

```rust
            out.push_str(&entry);
        }

        // Only the header fit — the budget is too small to say anything.
        (out.lines().count() > 1).then_some(out)
    }
```

to:

```rust
            out.push_str(&entry);
        }

        for line in self.wiki_pointer_lines() {
            if out.len() + line.len() > budget_chars {
                break;
            }
            out.push_str(&line);
        }

        // Only the header fit — the budget is too small to say anything.
        (out.lines().count() > 1).then_some(out)
    }
```

Add the standalone summary-extraction helper (near `render_file`, at the bottom of the non-test code):

```rust
/// Extracts just the `summary:` line from a `---`-delimited frontmatter
/// block, if present. Deliberately lenient — not a real YAML parser, just
/// enough structure-scanning for the one field this crate ever needs (see
/// `aivyx_tools::wiki`'s independent, more complete parser for the format
/// this reads; duplicated here rather than shared, per this crate's
/// dependency-free constraint).
fn wiki_summary(content: &str) -> Option<String> {
    let after_open = content.strip_prefix("---\n")?;
    let block_end = after_open.find("\n---")?;
    let block = &after_open[..block_end];
    block.lines().find_map(|line| {
        line.strip_prefix("summary: ")
            .map(|v| v.trim().trim_matches('"').to_string())
    })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-repomap`
Expected: all tests pass, including the 4 new ones.

- [ ] **Step 5: Run the full workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-repomap/src/lib.rs
git commit -m "Phase 11b: repo map wiki pointer section"
```

---

### Task 6: Live E2E verification and documentation

**Files:**
- Modify: `README.md` (new `--auto`-adjacent section documenting `/wiki`)
- Modify: `ROADMAP.md` (mark Phase 11b built)

Matches this project's own verification bar: every phase gets a real, live run through the actual binary before being marked done, not just unit tests. Use the same Lemonade-managed llama-server setup and pty-harness pattern from the Phase 10/12/11c acceptance work if a local model is available; otherwise this task's live-check steps should be run manually by whoever executes this plan.

- [ ] **Step 1: Manual live E2E — first run generates the full skeleton**

In a scratch git repo with at least one committed Rust file under `crates/<name>/` (or run this directly in `aivyx-coder`'s own worktree against a throwaway branch, since this project already has the exact multi-crate structure `/wiki` is designed around):

```bash
aivyx
# then type: /wiki
```

Expected, observed in the transcript: no permission modal is skipped or bypassed (every `write_file` still shows the normal diff-preview confirmation); one turn per page runs in sequence (`architecture-overview` plus one per crate); `docs/wiki/*.md` files exist afterward, each with `generated_at_commit`/`covers`/`summary` frontmatter stamped to the real current HEAD commit — confirm via `git log -1 --format=%H` matching each page's `generated_at_commit`.

- [ ] **Step 2: Manual live E2E — staleness-driven partial regeneration**

After Step 1, make a small change to exactly one crate (e.g. add a comment to a file under `crates/aivyx-core/src/`), commit it, then run `/wiki` again.

Expected: only `docs/wiki/aivyx-core.md` (and `architecture-overview.md` only if the change touched its curated `ARCHITECTURE_OVERVIEW_COVERS` paths, which a comment-only change under `crates/aivyx-core/src/` other than `agent.rs` should not) gets regenerated — confirm via the transcript showing only that page's instruction, and via each page's `generated_at_commit` in frontmatter (unaffected pages keep their old commit; the affected page's advances to the new HEAD).

- [ ] **Step 3: Manual live E2E — forced regeneration**

Run `/wiki aivyx-tui` (or any already-up-to-date page name).

Expected: that one page regenerates even though `stale_pages` wouldn't have flagged it — confirm via its `generated_at_commit` advancing to the current HEAD despite no relevant source changes since the last run.

- [ ] **Step 4: Manual live E2E — Ctrl+C mid-batch**

Force a multi-page regeneration (e.g. delete two wiki pages' frontmatter `generated_at_commit` lines by hand to force both stale, matching Task 2's "malformed frontmatter treated as stale" test), start `/wiki`, and press Ctrl+C partway through the first page's generation.

Expected: the in-flight page's turn is cancelled (matches existing interactive-mode Ctrl+C behavior — no `/wiki`-specific handling was added, and none should be needed); no further pages are attempted; running `/wiki` again afterward picks up exactly the pages that never got a chance to run (idempotent resume, no persisted queue state).

- [ ] **Step 5: Update README.md**

Find the "Autonomous mode" paragraph added by Phase 11c (`grep -n "Autonomous mode" README.md`). Add a new paragraph immediately after it:

```markdown
**Agent-maintained wiki** (`/wiki`, `/wiki <page>`): generates and keeps
`docs/wiki/*.md` up to date — one page per workspace crate plus
`architecture-overview.md` — using the agent's normal gated tools, no new
trust tier. Bare `/wiki` regenerates only pages whose covered source files
changed since they were last generated (tracked per-page, in each page's
frontmatter, against the commit it was generated at — not against
uncommitted changes); `/wiki <page>` forces one page regardless of
staleness. Every page write still goes through the standard confirmation
modal. The repo map lists existing pages (path + one-line summary) as
pointers so the model can `read_file` the relevant one on demand, at no new
token-budget cost. See ROADMAP.md's Phase 11b entry for the full design
rationale.
```

- [ ] **Step 6: Update ROADMAP.md**

Find the Phase 11b sketch (`grep -n "11b — Agent-maintained codebase wiki" ROADMAP.md`). Add a "Built and live-verified (date)" paragraph immediately after it, following the exact pattern Phase 11c's entry uses (a "Built and live-verified" paragraph after its own candidate-direction sketch): summarize what was built (the `aivyx-tools::wiki` staleness/frontmatter module, `Agent::run_wiki_turn`'s per-page `TurnPaused`-continuation reuse, the repo-map pointer section, the fixed-at-implementation-time `architecture-overview.md` coverage list as a disclosed deviation from "curated by the agent" framing), the test count added, and the results of the 4 live E2E checks from Steps 1-4 above. Also update Phase 11's own status line (`grep -n "11a shipped and live-verified" ROADMAP.md`) — it currently reads "11b and 11c not started," which is now stale on both counts (11c shipped 2026-07-13, 11b as of this task) — correct it to reflect both are shipped.

- [ ] **Step 7: Final full-workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add README.md ROADMAP.md
git commit -m "Phase 11b: docs + live E2E verification"
```

---

## Self-Review

**Spec coverage** — every section of the design doc maps to a task:
- Wiki structure + frontmatter format → Task 1 (types/parsing), Task 3 (skeleton).
- Staleness detection → Task 2.
- Command interception + per-page orchestration → Task 4.
- Repo-map integration → Task 5.
- Error handling (empty stale list, unrecognized forced page, page-turn failure, cancellation) → Task 4 (all four cases have dedicated tests).
- Testing strategy (real-git fixtures in aivyx-tools, parse_command tests mirroring council's, mock-backend Agent tests, repo-map render tests, one live E2E) → covered per-task plus Task 6.
- Non-goals (autonomous trust profile, preserved-region merging, cross-page graph, wiki versioning) — no task builds any of these; confirmed absent by design, not omission.

**Placeholder scan** — no "TBD"/"TODO"/"add appropriate error handling" anywhere in this plan; every step has complete, real code, including the two implementation-time refinements flagged explicitly in Global Constraints (pathspec syntax without `/**`, `architecture-overview.md`'s fixed-not-agent-recurated covers list) rather than left vague.

**Type consistency** — `PageSpec`/`StalePage`/`StaleReason`/`Frontmatter` (Task 1/2, `aivyx_tools::wiki`) are the exact types Task 3's `page_specs` and Task 4's `run_wiki_turn`/`run_wiki_turn_for_pages` consume, field-for-field. `WikiCommand`/`parse_command`/`WIKI_DIR`/`page_specs` (Task 3, `aivyx_core::wiki`) match exactly what Task 4's `Agent::run_wiki_turn` calls. `stale_pages`/`stamp_page`'s signatures (Task 2) match exactly how Task 4 calls them (`cwd`, `wiki_dir`, `specs`/`name`+`covers`, `cancellation` all line up).
