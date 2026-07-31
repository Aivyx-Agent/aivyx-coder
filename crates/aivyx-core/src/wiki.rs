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
/// starting with those letters is a normal turn).
pub fn parse_command(input: &str) -> Option<WikiCommand> {
    let page = crate::commands::parse_slash_command(input, "/wiki")?;
    if page.is_empty() {
        Some(WikiCommand::Batch)
    } else {
        Some(WikiCommand::Forced(page.to_string()))
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
