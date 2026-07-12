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
