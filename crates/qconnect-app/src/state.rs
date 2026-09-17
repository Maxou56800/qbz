use qconnect_core::{PendingActionSlot, QConnectQueueState, QConnectRendererState};

#[derive(Debug, Default)]
pub struct QconnectRuntimeState {
    pub renderer_generation: u64,
    pub deferred_renderer_command: Option<(u64, qconnect_protocol::RendererServerCommand)>,
    pub queue: QConnectQueueState,
    pub renderer: QConnectRendererState,
    pub pending: PendingActionSlot,
    pub transport_connected: bool,
    pub concurrency_canceled_action_uuid: Option<String>,
}
