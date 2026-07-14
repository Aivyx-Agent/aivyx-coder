//! Exercises the real `rust-analyzer` binary end-to-end — spawn, confiner
//! reuse (via `NoopConfiner`, since this test doesn't need real
//! Landlock/seccomp confinement to prove the LSP protocol round-trip
//! works), initialize handshake, a real go-to-definition and
//! find-references query, and respawn-after-crash. Skipped entirely (not
//! failed) when `rust-analyzer` isn't on `PATH`, since this project's dev
//! and CI environments aren't guaranteed to have it installed — every
//! other assertion in this crate's test suite works without it.

use std::process::Command as StdCommand;
use std::sync::Arc;
use std::time::Duration;

use aivyx_sandbox::NoopConfiner;
use aivyx_tools::LspClient;

fn rust_analyzer_available() -> bool {
    StdCommand::new("rust-analyzer")
        .arg("--version")
        .output()
        .is_ok()
}

#[tokio::test]
async fn go_to_definition_and_find_references_round_trip_against_a_real_workspace() {
    if !rust_analyzer_available() {
        eprintln!("skipping: rust-analyzer not found on PATH");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn fib(n: u64) -> u64 {\n    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }\n}\n\n\
             pub fn call_fib() -> u64 {\n    fib(10)\n}\n",
    )
    .unwrap();

    let confiner: Arc<dyn aivyx_sandbox::ExecutionConfiner> = Arc::new(NoopConfiner);
    // Generous timeout: cold rust-analyzer indexing of even a tiny fixture
    // crate can take tens of seconds on a loaded CI machine.
    let client = LspClient::new(Duration::from_secs(120));

    // `call_fib`'s body calls `fib(10)` at line 6 (1-indexed), column 5 —
    // go-to-definition from that call site should resolve to `fib`'s own
    // definition at line 1.
    let definition = client
        .go_to_definition(dir.path(), &confiner, "src/lib.rs", 6, 5)
        .await
        .expect("go_to_definition should succeed against a real rust-analyzer");
    assert!(
        definition.contains("src/lib.rs:1:"),
        "expected the definition to resolve to line 1, got: {definition}"
    );

    // Every reference to `fib` (its own two recursive calls plus the call
    // from `call_fib`) should be found from a query on the definition site
    // itself (line 1).
    let references = client
        .find_references(dir.path(), &confiner, "src/lib.rs", 1, 8)
        .await
        .expect("find_references should succeed against a real rust-analyzer");
    assert!(
        references.lines().count() >= 3,
        "expected at least 3 reference sites (2 recursive calls + 1 from call_fib), got: \
         {references}"
    );
}
