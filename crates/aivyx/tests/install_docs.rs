//! The install command in the README and the manual must match the archives
//! the release workflow builds. Since v0.6.5 the folder inside each archive
//! has no version (for the ACP registry's `cmd`), and the documented
//! `mv "aivyx-coder-$V-$T/aivyx-coder"` failed for every new user until
//! onboarding audit 3 (2026-11-12) caught it.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> String {
    std::fs::read_to_string(repo().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn the_release_workflow_names_archives_and_folders_as_the_docs_expect() {
    let workflow = read(".github/workflows/release.yml");
    assert!(
        workflow.contains(r#"ARCHIVE_NAME="aivyx-coder-${GITHUB_REF_NAME}-${{ matrix.stage_suffix }}""#),
        "the archive file name changed: update the install command in README.md and the manual"
    );
    assert!(
        workflow.contains(r#"STAGE_NAME="aivyx-coder-${{ matrix.stage_suffix }}""#),
        "the folder inside the archive changed: update the install command in README.md and the manual"
    );
}

#[test]
fn the_documented_install_command_extracts_the_binary_the_archive_holds() {
    for doc in ["README.md", "docs/manual/guide/02-install-and-first-run.md"] {
        let text = read(doc);
        assert!(
            text.contains(r#"releases/download/$V/aivyx-coder-$V-$T.tar.gz"#),
            "{doc}: the archive URL must be aivyx-coder-$V-$T.tar.gz"
        );
        assert!(
            text.contains(r#"--strip-components=1 "aivyx-coder-$T/aivyx-coder""#),
            "{doc}: extract the binary by its version-free path inside the archive"
        );
        assert!(
            !text.contains(r#"aivyx-coder-$V-$T/"#),
            "{doc}: no folder inside the archive carries the version"
        );
    }
}
