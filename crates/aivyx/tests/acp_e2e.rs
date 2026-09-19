use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{AuthMethod, InitializeRequest};
use agent_client_protocol::{AcpAgent, Client};
use std::str::FromStr;
use tempfile::tempdir;

/// Regression test for the whole-branch review finding this task addendum
/// exists to fix: an editor client must launch `aivyx-coder --acp` just to
/// receive the `initialize` response advertising the `terminal` auth
/// method -- and if that same launch silently wrote a default
/// `config.toml` (as the shared `Settings::load()` used to do for every
/// frontend, ACP included), the wizard the client then launched to
/// satisfy the auth method would always find the file already there and
/// refuse immediately, reporting exit-0 "success" for a gate that never
/// gated anything. Spawns a real `aivyx-coder --acp` process (same binary
/// Zed would launch) against a fresh, empty `XDG_CONFIG_HOME` with no
/// `config.toml` present, and asserts `session/new` genuinely fails with
/// ACP's `auth_required` error (code -32000) rather than succeeding.
#[tokio::test]
async fn acp_session_new_fails_with_auth_required_when_no_config_exists() {
    let bin = env!("CARGO_BIN_EXE_aivyx-coder");
    let cwd = tempdir().unwrap();
    let config_home = tempdir().unwrap();
    // No config.toml under `config_home` -- `Settings::config_path()`
    // resolves under `XDG_CONFIG_HOME/aivyx-coder/config.toml` on Linux,
    // so setting this env var (via `AcpAgent::from_str`'s leading
    // `NAME=value` parsing, same convention its own doc examples use)
    // gives the spawned process a controlled, guaranteed-empty config
    // directory without touching the real one.
    let command = format!(
        "XDG_CONFIG_HOME={} {bin} --acp",
        config_home.path().display()
    );
    let agent = AcpAgent::from_str(&command).expect("valid command");

    Client
        .builder()
        .name("aivyx-acp-e2e-test")
        .connect_with(agent, async |cx| {
            let init = cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await
                .expect("initialize should succeed even with no config yet");
            assert_eq!(init.protocol_version, ProtocolVersion::V1);
            assert!(
                !init.auth_methods.is_empty(),
                "initialize must still advertise the terminal auth method with no config \
                 present, expected client to run it before session/new can ever succeed"
            );

            let error = cx
                .build_session(cwd.path())
                .block_task()
                .run_until(async |_session| Ok(()))
                .await
                .expect_err(
                    "session/new must fail with auth_required when no config.toml exists -- \
                     succeeding here would mean the auth gate never actually gated anything",
                );
            assert_eq!(
                i32::from(error.code),
                -32000,
                "expected the real auth_required JSON-RPC error code on the wire, got {error:?}"
            );

            Ok(())
        })
        .await
        .expect("connection should complete without error");

    // Sanity check: no config.toml must have been created as a side
    // effect of this whole sequence -- `Settings::load_existing()` must
    // never write, unlike `Settings::load()`.
    assert!(
        !config_home
            .path()
            .join("aivyx-coder")
            .join("config.toml")
            .exists(),
        "the ACP path must never write a default config.toml"
    );
}

/// Drives the real, release-identical `aivyx --acp` binary over stdio — the
/// same integration point Zed itself would use — rather than the crate's
/// own translation-layer unit tests. Requires no LLM backend:
/// `initialize`/`session/new` never call the model.
///
/// Hermetic by construction: since `fb088cf` switched the `--acp` branch
/// from `Settings::load()` (writes a default `config.toml` on first run) to
/// `Settings::load_existing()` (never writes, returns `None` if absent),
/// this test must supply its own `config.toml` under a scratch
/// `XDG_CONFIG_HOME` rather than relying on the ambient environment already
/// happening to have one at `~/.config/aivyx-coder/config.toml` -- that
/// would only be true by coincidence on a given developer machine and false
/// on a clean CI runner. Also exercises the "config already exists ->
/// normal session" branch that `acp_session_new_fails_with_auth_required_when_no_config_exists`
/// deliberately does not cover.
#[tokio::test]
async fn acp_initialize_and_new_session_round_trip() {
    let bin = env!("CARGO_BIN_EXE_aivyx-coder");
    let cwd = tempdir().unwrap();
    let config_home = tempdir().unwrap();
    // Write a real config.toml via the crate's own serialization path
    // (`Settings::write_if_absent_at`, the same `write_to` internals
    // `Settings::load()` itself uses to write first-run defaults) rather
    // than hand-writing a TOML string, so this test can never drift out of
    // sync with the real `Settings` schema.
    let config_path = config_home.path().join("aivyx-coder").join("config.toml");
    aivyx_config::Settings::default()
        .write_if_absent_at(&config_path)
        .expect("failed to write test config.toml");

    // A dummy backend URL is fine here — this test never sends a prompt,
    // so the LLM backend is never actually contacted.
    let command = format!(
        "XDG_CONFIG_HOME={} {bin} --acp --base-url http://127.0.0.1:1/v1 --model test-model",
        config_home.path().display()
    );
    let agent = AcpAgent::from_str(&command).expect("valid command");

    Client
        .builder()
        .name("aivyx-acp-e2e-test")
        .connect_with(agent, async |cx| {
            let init = cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await
                .expect("initialize should succeed");
            assert_eq!(init.protocol_version, ProtocolVersion::V1);

            // Direct coverage of the auth_methods shape (not just
            // non-emptiness, which acp_session_new_fails_with_auth_required_when_no_config_exists
            // already checks): exactly the one `terminal` auth method
            // `terminal_auth_method()` builds, with the `id`/`args` a
            // client actually parses to know what to launch.
            assert_eq!(
                init.auth_methods.len(),
                1,
                "expected exactly one advertised auth method, got {:?}",
                init.auth_methods
            );
            match &init.auth_methods[0] {
                AuthMethod::Terminal(terminal) => {
                    assert_eq!(terminal.id.0.as_ref(), "setup");
                    assert_eq!(terminal.args, vec!["--setup".to_string()]);
                }
                other => panic!("expected AuthMethod::Terminal, got {other:?}"),
            }

            cx.build_session(cwd.path())
                .block_task()
                .run_until(async |session| {
                    assert!(!session.session_id().0.as_ref().is_empty());
                    Ok(())
                })
                .await
        })
        .await
        .expect("connection should complete without error");
}
