use aivyx_sandbox::{PermissionPrompter, PermissionRequest, UserResponse};
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

/// A permission prompt in flight, plus the channel back to whichever
/// `ConfirmationGate::check` call is awaiting the answer.
///
/// Public so `PermissionModalReceiver`/`run` can name it in their
/// signatures, but its fields are crate-private — callers outside this
/// crate (i.e. `main.rs`) can only move it opaquely from
/// `permission_channel`'s receiver into `run`, never construct or inspect
/// one themselves.
pub struct ModalRequest {
    pub(crate) request: PermissionRequest,
    pub(crate) reply_tx: oneshot::Sender<UserResponse>,
}

pub type PermissionModalReceiver = mpsc::UnboundedReceiver<ModalRequest>;

/// The `PermissionPrompter` side of the bridge: `ConfirmationGate::check`
/// runs on the background agent task and awaits `prompt()`, but the modal
/// itself has to render in the TUI's render-loop task — so this just hands
/// the request across a channel and waits for the render loop to reply.
pub struct TuiPrompter {
    tx: mpsc::UnboundedSender<ModalRequest>,
}

#[async_trait]
impl PermissionPrompter for TuiPrompter {
    async fn prompt(&self, request: &PermissionRequest) -> UserResponse {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ModalRequest {
                request: request.clone(),
                reply_tx,
            })
            .is_err()
        {
            // Render loop is gone (app shutting down) — fail closed.
            return UserResponse::Deny;
        }
        reply_rx.await.unwrap_or(UserResponse::Deny)
    }
}

/// Builds a `TuiPrompter`/receiver pair. The prompter goes into a
/// `ConfirmationGate`; the receiver is passed to `run` so its render loop
/// can pick up incoming requests as a third `tokio::select!` branch.
pub fn permission_channel() -> (TuiPrompter, PermissionModalReceiver) {
    let (tx, rx) = mpsc::unbounded_channel();
    (TuiPrompter { tx }, rx)
}
