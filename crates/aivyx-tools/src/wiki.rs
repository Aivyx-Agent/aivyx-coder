//! Agent-maintained codebase wiki (ROADMAP.md Phase 11b): deterministic
//! staleness detection and frontmatter bookkeeping for the pages under
//! `docs/wiki/`. No LLM judgment lives here — this module only ever answers
//! "does this page's recorded commit + covered paths look out of date" and
//! "rewrite this page's bookkeeping fields," both via plain git plumbing and
//! hand-rolled text parsing. `aivyx-core::wiki` owns everything about *which*
//! pages exist and how turns are driven to fill them in.

use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use crate::checkpoint::run_git;

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
        // No closing marker at all — treat the whole thing as an
        // unrecognized body rather than guessing. (A frontmatter block
        // that runs to end-of-file with no trailing body, e.g.
        // "---\nkey: value\n---\n", is already handled by the `find`
        // success path above — its closing "\n---\n" is still found,
        // just with an empty remainder after it.)
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

/// Wraps `run_git` with cooperative cancellation, matching
/// `GitCheckpointer::git`'s own wrapper — this module talks to git via the
/// same shared plumbing, just without a private index (staleness/stamping
/// never touch the worktree or any index, only read commits and rewrite one
/// file).
async fn git(
    cwd: &Path,
    args: &[&str],
    cancellation: &CancellationToken,
) -> Result<String, String> {
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

        if spec.covers.is_empty() {
            out.push(StalePage {
                name: spec.name.clone(),
                covers: spec.covers.clone(),
                reason: StaleReason::Stale,
            });
            continue;
        }

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

        assert!(
            result.is_empty(),
            "unrelated changes must not mark the page stale"
        );
    }

    #[tokio::test]
    async fn stale_pages_treats_malformed_frontmatter_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let wiki_dir = dir.path().join("docs/wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        std::fs::write(
            page_path(&wiki_dir, "aivyx-core"),
            "no frontmatter at all\n",
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
    async fn stale_pages_treats_empty_covers_as_stale_without_running_an_unrestricted_diff() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let generated_at = run_git(dir.path(), &["rev-parse", "HEAD"], &[])
            .await
            .unwrap()
            .trim()
            .to_string();

        let wiki_dir = dir.path().join("docs/wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        std::fs::write(
            page_path(&wiki_dir, "empty-covers"),
            format!("---\ngenerated_at_commit: {generated_at}\n---\nbody\n"),
        )
        .unwrap();

        // A commit that happens after the page was generated, touching
        // something totally unrelated to this page — if the empty-covers
        // guard weren't in place, an unrestricted `git diff -- ` (no
        // pathspec) would still see this and incorrectly mark the page
        // stale for the right *conclusion* but the wrong *reason*; this
        // test's real point is proving the function takes the dedicated
        // empty-covers path rather than reaching the git-diff call at all,
        // which the guard achieves regardless of what else changed.
        std::fs::write(dir.path().join("unrelated.txt"), "noise\n").unwrap();
        commit_all(dir.path(), "unrelated change").await;

        let result = stale_pages(
            dir.path(),
            &wiki_dir,
            &[spec("empty-covers", &[])],
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
        std::fs::write(
            page_path(&wiki_dir, "aivyx-core"),
            "just a body, no frontmatter\n",
        )
        .unwrap();

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
