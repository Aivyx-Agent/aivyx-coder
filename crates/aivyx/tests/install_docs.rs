//! The install command in the README and the manual, the release config
//! that produces what it downloads, and the ACP registry entry's paths
//! must agree. A five-line hand-written install snippet once drifted from
//! the release archives and failed for every new user from v0.6.5 until
//! onboarding audit 3 (2026-11-12); since 2026-11-13 cargo-dist builds the
//! release and a one-line installer, and these tests keep the three in step.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> String {
    std::fs::read_to_string(repo().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

const INSTALLER: &str = "https://github.com/Aivyx-Agent/aivyx-coder/releases/latest/download/aivyx-coder-installer.sh";

/// The targets the release builds, as cargo-dist names them.
const TARGETS: [&str; 2] = ["x86_64-unknown-linux-musl", "aarch64-apple-darwin"];

#[test]
fn the_release_config_builds_the_installer_and_the_archives() {
    let dist = read("dist-workspace.toml");
    assert!(dist.contains(r#"installers = ["shell"]"#), "the shell installer is gone");
    assert!(dist.contains(r#"unix-archive = ".tar.gz""#), "the registry reads .tar.gz");
    for target in TARGETS {
        assert!(dist.contains(&format!("\"{target}\"")), "{target} is no longer built");
    }
    let manifest = read("crates/aivyx/Cargo.toml");
    assert!(
        manifest.contains("name = \"aivyx-coder\""),
        "the package name names the installer and archives (aivyx-coder-installer.sh)"
    );
}

#[test]
fn the_docs_install_with_the_installer() {
    for doc in ["README.md", "docs/manual/guide/02-install-and-first-run.md"] {
        let text = read(doc);
        assert!(text.contains(INSTALLER), "{doc}: install with {INSTALLER}");
        assert!(!text.contains("--strip-components"), "{doc}: the old snippet is back");
    }
}

#[test]
fn the_registry_entry_points_into_the_archives_cargo_dist_builds() {
    let entry = read("docs/registry-submission/aivyx-coder/agent.json");
    for target in TARGETS {
        assert!(
            entry.contains(&format!(r#""cmd": "./aivyx-coder-{target}/aivyx-coder""#)),
            "agent.json: the binary is at aivyx-coder-{target}/aivyx-coder in the archive"
        );
        assert!(
            entry.contains(&format!("aivyx-coder-{target}.tar.gz")),
            "agent.json: the archive is aivyx-coder-{target}.tar.gz"
        );
    }
}
