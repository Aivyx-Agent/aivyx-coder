//! ACP server frontend for aivyx-coder's `Agent` core. See
//! `docs/superpowers/specs/2026-07-20-acp-editor-integration-design.md`.

use agent_client_protocol::schema::v1::{AgentCapabilities, InitializeRequest, InitializeResponse};
use agent_client_protocol::{Agent, Dispatch, Result, Stdio};

mod prompter;
mod translate;

pub use prompter::{AcpPrompter, DeferredPrompter, PrompterInstaller, deferred_prompter};

/// Runs the ACP server loop over stdin/stdout until the connection
/// closes. Only `initialize` is handled so far — `NewSessionRequest`/
/// `PromptRequest`/`SetSessionModeRequest` are added in Task 5.
pub async fn run() -> Result<()> {
    Agent
        .builder()
        .name("aivyx")
        .on_receive_request(
            async move |initialize: InitializeRequest, responder, _connection| {
                responder.respond(
                    InitializeResponse::new(initialize.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_dispatch(
            async move |message: Dispatch, cx| {
                message.respond_with_error(
                    agent_client_protocol::util::internal_error("not yet implemented"),
                    cx,
                )
            },
            agent_client_protocol::on_receive_dispatch!(),
        )
        .connect_to(Stdio::new())
        .await
}
