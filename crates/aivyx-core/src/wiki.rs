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

/// Build manifests that describe a project's overall shape, in the order
/// they're listed on `architecture-overview`.
const MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
];

/// Common top-level entry points.
const ENTRY_POINTS: &[&str] = &[
    "src/main.rs",
    "src/lib.rs",
    "main.go",
    "src/index.ts",
    "src/index.js",
    "src/main.ts",
    "src/main.py",
    "main.py",
];

/// `architecture-overview.md`'s coverage for the project at `cwd`: the
/// structural files it actually has — its build manifests, its top-level
/// entry points and each workspace crate's `src/main.rs` — not the whole
/// repo. Found on disk (this used to be a fixed list of aivyx-coder's own
/// files, so `/wiki` in any other project asked the model to document
/// files that weren't there). Empty when nothing is recognised.
fn architecture_overview_covers(cwd: &Path) -> Vec<String> {
    let mut covers: Vec<String> = MANIFESTS
        .iter()
        .chain(ENTRY_POINTS)
        .filter(|path| cwd.join(path).is_file())
        .map(|path| path.to_string())
        .collect();
    let mut binaries: Vec<String> = workspace_crates(cwd)
        .into_iter()
        .map(|name| format!("crates/{name}/src/main.rs"))
        .filter(|path| cwd.join(path).is_file())
        .collect();
    binaries.sort();
    covers.extend(binaries);
    covers
}

/// Directories under `cwd/crates` that hold a `Cargo.toml`, sorted.
fn workspace_crates(cwd: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(cwd.join("crates"))
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().join("Cargo.toml").is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// What `/wiki` says when `page_specs` finds nothing to document.
pub const NOTHING_TO_DOCUMENT: &str = "/wiki found nothing to document here: it looks for \
     a build manifest (Cargo.toml, package.json, pyproject.toml, go.mod, ...), a top-level \
     entry point (src/main.rs, main.go, src/index.ts, ...) or crates under crates/. Run it \
     from the project's root.";

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

/// The project's full page skeleton: `architecture-overview` (when
/// `architecture_overview_covers` finds anything) plus one page per
/// workspace crate, discovered from `cwd/crates/*` (a directory containing
/// a `Cargo.toml`), so a new crate automatically gets a page. The
/// directory name is used as the page name without parsing `Cargo.toml`.
/// Empty when the project has nothing recognisable: `/wiki` then says so
/// (`NOTHING_TO_DOCUMENT`) instead of claiming the wiki is up to date.
pub fn page_specs(cwd: &Path) -> Vec<PageSpec> {
    let mut specs = Vec::new();
    let overview = architecture_overview_covers(cwd);
    if !overview.is_empty() {
        specs.push(PageSpec {
            name: "architecture-overview".to_string(),
            covers: overview,
        });
    }

    for name in workspace_crates(cwd) {
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
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
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
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let specs = page_specs(dir.path());
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "architecture-overview");
        assert_eq!(specs[0].covers, vec!["Cargo.toml"]);
    }

    #[test]
    fn the_overview_covers_only_files_this_project_has() {
        // A single-crate Rust project like aivyx-deploy: none of
        // aivyx-coder's own files (the 2026-11-13 failure).
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        for file in ["Cargo.toml", "src/main.rs", "README.md"] {
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        assert_eq!(
            architecture_overview_covers(dir.path()),
            vec!["Cargo.toml", "src/main.rs"]
        );

        // A workspace: the root manifest plus each binary crate's main.rs,
        // sorted; a library crate adds no entry point.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        for (name, file) in [("zed", "main.rs"), ("app", "main.rs"), ("core", "lib.rs")] {
            let src = dir.path().join("crates").join(name).join("src");
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(src.parent().unwrap().join("Cargo.toml"), "[package]\n").unwrap();
            std::fs::write(src.join(file), "").unwrap();
        }
        assert_eq!(
            architecture_overview_covers(dir.path()),
            vec!["Cargo.toml", "crates/app/src/main.rs", "crates/zed/src/main.rs"]
        );

        // A Node project.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        for file in ["package.json", "src/index.ts"] {
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        assert_eq!(
            architecture_overview_covers(dir.path()),
            vec!["package.json", "src/index.ts"]
        );
    }

    #[test]
    fn a_folder_with_nothing_recognisable_has_no_pages() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        assert!(page_specs(dir.path()).is_empty());
    }
}
