use agent_client_protocol::schema::v1::InitializeRequest;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, Client};
use std::str::FromStr;
use tempfile::tempdir;

/// Drives the real, release-identical `aivyx --acp` binary over stdio — the
/// same integration point Zed itself would use — rather than the crate's
/// own translation-layer unit tests. Requires no LLM backend:
/// `initialize`/`session/new` never call the model.
#[tokio::test]
async fn acp_initialize_and_new_session_round_trip() {
    let bin = env!("CARGO_BIN_EXE_aivyx-coder");
    let cwd = tempdir().unwrap();
    // A dummy backend URL is fine here — this test never sends a prompt,
    // so the LLM backend is never actually contacted.
    let command = format!(
        "{bin} --acp --base-url http://127.0.0.1:1/v1 --model test-model"
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
