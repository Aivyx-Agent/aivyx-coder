use agent_client_protocol::schema::v1::InitializeRequest;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, Client};
use std::str::FromStr;

#[tokio::test]
async fn initialize_round_trips_over_the_real_binary() {
    let bin = env!("CARGO_BIN_EXE_aivyx_acp_test_stub");
    let agent = AcpAgent::from_str(bin).expect("valid command");

    let response = Client
        .builder()
        .name("aivyx-acp-test-client")
        .connect_with(agent, async |connection| {
            connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await
        })
        .await
        .expect("initialize should succeed");

    assert_eq!(response.protocol_version, ProtocolVersion::V1);
}
