#[tokio::main]
async fn main() -> agent_client_protocol::Result<()> {
    aivyx_acp::run().await
}
