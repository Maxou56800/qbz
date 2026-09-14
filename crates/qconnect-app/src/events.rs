use async_trait::async_trait;
use qconnect_core::{QConnectQueueState, QConnectRendererState, RendererCommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session::QconnectLifecycleState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum QconnectAppEvent {
    TransportConnected,
    TransportDisconnected,
    QueueUpdated(QConnectQueueState),
    RendererUpdated(QConnectRendererState),
    RendererCommandApplied {
        command: RendererCommand,
        state: QConnectRendererState,
    },
    PendingActionStarted {
        uuid: String,
    },
    PendingActionCompleted {
        uuid: String,
    },
    PendingActionTimedOut {
        uuid: String,
        timeout_ms: u64,
    },
    PendingActionCanceledByConcurrentRemoteEvent {
        pending_uuid: String,
        remote_action_uuid: String,
    },
    QueueErrorIgnoredByConcurrency {
        action_uuid: String,
    },
    QueueResyncTriggered,
    /// Renderer per-track playback failure (PlaybackErrorMessage). The frontend
    /// maps streamable/network/not-found failures on the current item to an
    /// auto-skip; other types surface a toast only.
    PlaybackError {
        queue_item_id: u64,
        error_type: qconnect_protocol::ErrorType,
        queue_version: Option<qconnect_core::QueueVersion>,
    },
    /// Session management event from server (types 81-87, 97-101).
    /// These don't affect the queue reducer but provide session topology info.
    SessionManagementEvent {
        message_type: String,
        payload: Value,
    },
    /// Active peer renderer went silent >=12s while PLAYING (liveness watchdog).
    RendererUnreachable {
        renderer_id: i32,
    },
    /// Active peer renderer left gracefully (status == ACTIVE_DISCONNECTED).
    RendererDisconnected {
        renderer_id: i32,
    },
    /// Post-reconnect resync has been issued (safe-to-replay advisory).
    ResyncComplete,
    /// Granular connection lifecycle changed (Connecting/Reconnecting/Connected/Exhausted/Off).
    LifecycleChanged {
        state: QconnectLifecycleState,
    },
    /// Pass-through diagnostic (inbound-event preview, cloud error, max-reconnect, ...).
    /// `channel` is the legacy Tauri channel suffix so the mapper is mechanical.
    Diagnostic {
        channel: String,
        level: String,
        payload: Value,
    },
}

#[async_trait]
pub trait QconnectEventSink: Send + Sync {
    /// Observation only; hosts return None after their session is fenced.
    fn playback_event(&self) -> Option<qbz_player::player::PlaybackEvent> {
        None
    }

    /// Command execution has an acknowledgement distinct from observations.
    /// Hosts override this to propagate engine/authority failures to reporting.
    async fn execute_renderer_command(
        &self, command: &RendererCommand, state: &QConnectRendererState,
    ) -> Result<(), String> {
        self.on_event(QconnectAppEvent::RendererCommandApplied {
            command: command.clone(), state: state.clone(),
        }).await;
        Ok(())
    }

    async fn on_event(&self, event: QconnectAppEvent);
}

#[derive(Debug, Clone, Default)]
pub struct NoOpEventSink;

#[async_trait]
impl QconnectEventSink for NoOpEventSink {
    async fn on_event(&self, _event: QconnectAppEvent) {}
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::*;

    #[derive(Debug, Default, Clone)]
    struct TestSink {
        events: Arc<Mutex<Vec<QconnectAppEvent>>>,
    }

    #[async_trait]
    impl QconnectEventSink for TestSink {
        async fn on_event(&self, event: QconnectAppEvent) {
            self.events.lock().await.push(event);
        }
    }

    /// Slice 3: the five new liveness/lifecycle/diagnostic variants flow through
    /// the `QconnectEventSink` trait. A sink receives each one verbatim.
    #[tokio::test]
    async fn new_variants_flow_through_sink() {
        let sink = TestSink::default();
        sink.on_event(QconnectAppEvent::RendererUnreachable { renderer_id: 7 })
            .await;
        sink.on_event(QconnectAppEvent::RendererDisconnected { renderer_id: 9 })
            .await;
        sink.on_event(QconnectAppEvent::ResyncComplete).await;
        sink.on_event(QconnectAppEvent::LifecycleChanged {
            state: QconnectLifecycleState::Reconnecting,
        })
        .await;
        sink.on_event(QconnectAppEvent::Diagnostic {
            channel: "qconnect:cloud_error".to_string(),
            level: "warning".to_string(),
            payload: serde_json::json!({ "code": 13 }),
        })
        .await;

        let events = sink.events.lock().await.clone();
        assert_eq!(events.len(), 5);
        assert!(matches!(
            events[0],
            QconnectAppEvent::RendererUnreachable { renderer_id: 7 }
        ));
        assert!(matches!(
            events[1],
            QconnectAppEvent::RendererDisconnected { renderer_id: 9 }
        ));
        assert!(matches!(events[2], QconnectAppEvent::ResyncComplete));
        assert!(matches!(
            events[3],
            QconnectAppEvent::LifecycleChanged {
                state: QconnectLifecycleState::Reconnecting
            }
        ));
        assert!(matches!(
            &events[4],
            QconnectAppEvent::Diagnostic { channel, level, .. }
                if channel == "qconnect:cloud_error" && level == "warning"
        ));
    }

    /// The lifecycle state must serialize snake_case so the Tauri mapper's
    /// `{"state": <serialized>}` payload is byte-identical to the prior raw
    /// `qconnect:status_changed` emit.
    #[test]
    fn lifecycle_state_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(QconnectLifecycleState::Reconnecting).unwrap(),
            serde_json::json!("reconnecting")
        );
        assert_eq!(
            serde_json::to_value(QconnectLifecycleState::Connected).unwrap(),
            serde_json::json!("connected")
        );
        assert_eq!(
            serde_json::to_value(QconnectLifecycleState::Exhausted).unwrap(),
            serde_json::json!("exhausted")
        );
    }
}
