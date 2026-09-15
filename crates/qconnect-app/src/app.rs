use std::{
    collections::VecDeque,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use qconnect_core::{
    apply_event, apply_renderer_command, telemetry, PendingCorrelation, PendingQueueAction,
    QConnectQueueState, QConnectRendererState, QueueEvent, QueueItem, QueueVersion,
    RendererCommand,
};
use qconnect_protocol::{
    build_qconnect_outbound_envelope, build_qconnect_renderer_outbound_envelope,
    decode_playback_error, parse_inbound_event, InboundEnvelope, QueueCommand, QueueCommandType,
    QueueEventType, QueueServerEvent, RendererBufferState, RendererCommandType, RendererReport,
    RendererReportType, RendererServerCommand,
};
use qconnect_transport_ws::{TransportEvent, WsTransport, WsTransportConfig};
use serde_json::Value;
use tokio::sync::Mutex;
use uuid::Uuid;

use async_trait::async_trait;
use qbz_player::player::PlaybackBufferState;

use crate::renderer::{PLAYING_STATE_PAUSED, PLAYING_STATE_STOPPED};
use crate::session::{
    compute_connection_state, deferred_join_reason, is_local_renderer_active,
    is_peer_renderer_active, normalize_active_renderer_id, refresh_local_renderer_id,
    should_arm_renderer_watchdog, should_reask_queue_state, LocalIdentity,
    QconnectFileAudioQualitySnapshot, QconnectLifecycleState, QconnectRendererInfo, RendererStatus,
    ServerActiveState, JOIN_SESSION_REASON_CONTROLLER_REQUEST, PLAYING_STATE_PLAYING,
    PLAYING_STATE_UNKNOWN, QCONNECT_RENDERER_LOST_TIMEOUT_MS,
};
use crate::{
    build_renderer_playback_report, ensure_session_renderer_state,
    sync_session_renderer_active_flags, QconnectAppError, QconnectAppEvent, QconnectEventSink,
    QconnectRemoteSyncState, QconnectRuntimeState, RendererPlaybackSnapshot,
};

pub struct QconnectApp<TTransport, TSink>
where
    TTransport: WsTransport,
    TSink: QconnectEventSink,
{
    transport: Arc<TTransport>,
    sink: Arc<TSink>,
    state: Arc<Mutex<QconnectRuntimeState>>,
    /// Cross-frontend remote-sync accumulator (session topology, renderer-state
    /// cache, materialization cache, load-attempt dedup, watchdog epoch). Held
    /// behind its OWN Mutex, disjoint from `state`: the two are never co-locked,
    /// so the renderer-report hot path never serializes against session/watchdog
    /// work and there is no deadlock edge against `clear_pending_if_matches`
    /// (which locks `state`). The session loop (slice 5) and the renderer engine
    /// (slice 6) both lock THIS one, exactly as they share it today via the Tauri
    /// adapter. See `MASTER-qconnect-to-slint-plan.md` §0 (one Mutex, not two).
    sync: Arc<Mutex<QconnectRemoteSyncState>>,
    /// Timers belong to this app instance, not to the process runtime. Keeping
    /// their handles here lets disconnect/retirement abort them synchronously
    /// before an old sink (and its renderer engine/stream feeder) can outlive
    /// the authority that created it.
    background_tasks: Arc<StdMutex<BackgroundTasks>>,
    renderer_commands: Arc<Mutex<()>>,
}

#[derive(Default)]
struct BackgroundTasks {
    accepting: bool,
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl<TTransport, TSink> Clone for QconnectApp<TTransport, TSink>
where
    TTransport: WsTransport,
    TSink: QconnectEventSink,
{
    fn clone(&self) -> Self {
        Self {
            transport: Arc::clone(&self.transport),
            sink: Arc::clone(&self.sink),
            state: Arc::clone(&self.state),
            sync: Arc::clone(&self.sync),
            background_tasks: Arc::clone(&self.background_tasks),
            renderer_commands: Arc::clone(&self.renderer_commands),
        }
    }
}

impl<TTransport, TSink> QconnectApp<TTransport, TSink>
where
    TTransport: WsTransport + 'static,
    TSink: QconnectEventSink + 'static,
{
    const PENDING_ACTION_TIMEOUT_MS: u64 = 10_000;

    /// Construct the app. `sync` is the shared remote-sync accumulator handle:
    /// the Tauri adapter builds it, hands a clone to its event sink / service
    /// loop, and passes the SAME `Arc` here so there is exactly one lock backing
    /// the session/liveness/renderer paths. The Slint adapter will do the same.
    pub fn new(
        transport: Arc<TTransport>,
        sink: Arc<TSink>,
        sync: Arc<Mutex<QconnectRemoteSyncState>>,
    ) -> Self {
        Self {
            transport,
            sink,
            renderer_commands: Arc::new(Mutex::new(())),
            state: Arc::new(Mutex::new(QconnectRuntimeState::default())),
            sync,
            background_tasks: Arc::new(StdMutex::new(BackgroundTasks {
                accepting: true,
                handles: Vec::new(),
            })),
        }
    }

    pub fn state_handle(&self) -> Arc<Mutex<QconnectRuntimeState>> {
        Arc::clone(&self.state)
    }

    /// Shared handle to the remote-sync accumulator. The adapter uses this so its
    /// event sink, service loop, and CoreBridge helpers lock the very same Mutex
    /// the app's own session/watchdog methods lock.
    pub fn sync_handle(&self) -> Arc<Mutex<QconnectRemoteSyncState>> {
        Arc::clone(&self.sync)
    }

    pub fn subscribe_transport_events(&self) -> tokio::sync::broadcast::Receiver<TransportEvent> {
        self.transport.subscribe()
    }

    pub async fn connect(&self, config: WsTransportConfig) -> Result<(), QconnectAppError> {
        self.transport.connect(config).await?;
        self.set_background_tasks_accepting(true);
        {
            let mut state = self.state.lock().await;
            state.transport_connected = true;
        }
        self.sink
            .on_event(QconnectAppEvent::TransportConnected)
            .await;
        Ok(())
    }

    pub async fn disconnect(&self) -> Result<(), QconnectAppError> {
        self.abort_background_tasks().await;
        let transport_result = self.transport.disconnect().await;
        {
            let mut state = self.state.lock().await;
            state.transport_connected = false;
            state.pending.clear();
        }
        self.sink
            .on_event(QconnectAppEvent::TransportDisconnected)
            .await;
        transport_result.map_err(Into::into)
    }

    pub async fn queue_state_snapshot(&self) -> QConnectQueueState {
        self.state.lock().await.queue.clone()
    }

    pub async fn renderer_state_snapshot(&self) -> QConnectRendererState {
        self.state.lock().await.renderer.clone()
    }

    pub async fn build_queue_command(
        &self,
        command_type: QueueCommandType,
        payload: Value,
    ) -> QueueCommand {
        let version_ref = self.state.lock().await.queue.version;
        QueueCommand::new(command_type, self.next_action_uuid(), version_ref, payload)
    }

    /// Publish an extension as an append, preserving the cloud's current item,
    /// clock and shuffle. New selections and takeovers still load the full queue.
    pub async fn build_local_queue_sync_command(
        &self,
        track_ids: &[u64],
        start_index: usize,
        allow_append: bool,
    ) -> QueueCommand {
        let observed = self.sink.playback_event();
        let state = self.state.lock().await;
        let queue = &state.queue;
        let append = allow_append
            && !queue.shuffle_mode
            && !queue.queue_items.is_empty()
            && queue.queue_items.len() < track_ids.len()
            && queue
                .queue_items
                .iter()
                .zip(track_ids)
                .all(|(item, id)| item.track_id == *id)
            // A repeated existing ID can also describe a middle insertion;
            // catalog IDs alone cannot preserve occurrence identity there.
            && !track_ids[queue.queue_items.len()..]
                .iter()
                .any(|id| queue.queue_items.iter().any(|item| item.track_id == *id))
            && queue.queue_items.get(start_index).is_some_and(|item| {
                state
                    .renderer
                    .current_track
                    .as_ref()
                    .is_some_and(|current| {
                        current.queue_item_id == item.queue_item_id
                            && current.track_id == item.track_id
                    })
            })
            && observed.as_ref().is_some_and(|event| {
                event.buffer_state == PlaybackBufferState::Ready
                    && track_ids.get(start_index) == Some(&event.track_id)
            });
        let (command_type, payload) = if append {
            (
                QueueCommandType::CtrlSrvrQueueAddTracks,
                serde_json::json!({
                    "track_ids": &track_ids[queue.queue_items.len()..],
                    "context_uuid": Uuid::new_v4().to_string(),
                    "autoplay_reset": false, "autoplay_loading": false,
                }),
            )
        } else {
            (
                QueueCommandType::CtrlSrvrQueueLoadTracks,
                serde_json::json!({
                    "track_ids": track_ids, "queue_position": start_index,
                    "shuffle_mode": false, "shuffle_pivot_index": start_index,
                    "context_uuid": Uuid::new_v4().to_string(),
                    "autoplay_reset": true, "autoplay_loading": false,
                }),
            )
        };
        QueueCommand::new(
            command_type,
            self.next_action_uuid(),
            queue.version,
            payload,
        )
    }

    pub async fn send_queue_command(
        &self,
        command: QueueCommand,
    ) -> Result<String, QconnectAppError> {
        // Enforce advertised restrictions at the shared send boundary too:
        // queued UI callbacks and non-Qt callers must not bypass the lock or
        // occupy a pending slot with a command the renderer cannot execute.
        if matches!(
            command.command_type,
            QueueCommandType::CtrlSrvrSetVolume | QueueCommandType::CtrlSrvrMuteVolume
        ) {
            let sync = self.sync.lock().await;
            let renderer_id = command
                .payload
                .get("renderer_id")
                .and_then(Value::as_i64)
                .and_then(|id| i32::try_from(id).ok())
                .or(sync.session.active_renderer_id);
            if sync.session.renderers.iter().any(|info| {
                Some(info.renderer_id) == renderer_id && !crate::renderer_allows_remote_volume(info)
            }) {
                return Err(QconnectAppError::RemoteVolumeRestricted);
            }
        }
        let action_uuid = command.action_uuid.clone();
        let is_set_active_renderer_action = matches!(
            command.command_type,
            QueueCommandType::CtrlSrvrSetActiveRenderer
        );
        let is_queue_load_tracks_action = matches!(
            command.command_type,
            QueueCommandType::CtrlSrvrQueueLoadTracks
        );
        let pending = PendingQueueAction {
            uuid: action_uuid.clone(),
            queue_version_ref: command.queue_version_ref,
            emit_result_event: true,
            is_ask_for_state_action: matches!(
                command.command_type,
                QueueCommandType::CtrlSrvrAskForQueueState
            ),
            is_transport_control_action: matches!(
                command.command_type,
                QueueCommandType::CtrlSrvrSetPlayerState
            ),
            is_set_loop_mode_action: matches!(
                command.command_type,
                QueueCommandType::CtrlSrvrSetLoopMode
            ),
            is_set_volume_action: matches!(
                command.command_type,
                QueueCommandType::CtrlSrvrSetVolume
            ),
            is_set_active_renderer_action,
            is_queue_load_tracks_action,
            expected_active_renderer_id: if is_set_active_renderer_action {
                pending_active_renderer_id_from_payload(&command.payload)
            } else {
                None
            },
            concurrency_error: false,
            sent_at_ms: now_ms(),
        };

        // Validate/encode before reserving the slot: a malformed command must
        // not strand all later actions behind an unsent request.
        let command_type = command.command_type;
        let envelope = build_qconnect_outbound_envelope(command)?;
        let send_without_pending = {
            let mut state = self.state.lock().await;
            // These controls do not mutate the queue or carry an action UUID
            // on the wire. Mac sends volume/loop/renderer selection directly;
            // its noWait player-state/state-query actions still wait behind a
            // current queue action. QBZ deliberately admits those two during a
            // read as well, to keep takeover controls responsive. This is a
            // local scheduling adaptation, not a claim of identical scheduling.
            // Keep the read's UUID/timeout and never bypass a real queue write.
            let bypass = state.pending.current().is_some_and(|action| {
                action.is_ask_for_state_action
                    && matches!(
                        command_type,
                        QueueCommandType::CtrlSrvrSetPlayerState
                            | QueueCommandType::CtrlSrvrSetActiveRenderer
                            | QueueCommandType::CtrlSrvrAskForRendererState
                            | QueueCommandType::CtrlSrvrSetVolume
                            | QueueCommandType::CtrlSrvrMuteVolume
                            | QueueCommandType::CtrlSrvrSetLoopMode
                    )
            }) || (state.pending.current().is_none()
                && matches!(command_type, QueueCommandType::CtrlSrvrMuteVolume));
            // Mute is a direct control in the official client, with no action
            // UUID response. Do not invent an uncompletable pending action for
            // it, even when no queue read is present. Real writes still fence it.
            if !bypass {
                if let Some(active) = state.pending.current() {
                    log::warn!(
                        "[QConnect] Command {command_type:?} blocked by pending {} (age_s={})",
                        if active.is_ask_for_state_action {
                            "queue-state read"
                        } else {
                            "action"
                        },
                        now_ms().saturating_sub(active.sent_at_ms) / 1000,
                    );
                }
                state.pending.start(pending)?;
            }
            bypass
        };

        if let Err(err) = self.transport.send(envelope).await {
            self.clear_pending_if_matches(&action_uuid).await;
            return Err(err.into());
        }

        if send_without_pending {
            log::debug!("[QConnect] Sent {command_type:?} without replacing pending queue state");
            return Ok(action_uuid);
        }

        self.sink
            .on_event(QconnectAppEvent::PendingActionStarted {
                uuid: action_uuid.clone(),
            })
            .await;
        self.spawn_pending_timeout_watch(action_uuid.clone());
        Ok(action_uuid)
    }

    pub async fn send_renderer_report_command(
        &self,
        report: RendererReport,
    ) -> Result<(), QconnectAppError> {
        self.send_renderer_report(report).await
    }

    /// Send the exact controller-requested renderer JoinSession report used by
    /// delegated handoff and reconnect. Hosts supply only their serialized
    /// device-info projection; queue version and initial wire state stay shared
    /// so Qt and qbzd cannot drift independently.
    pub async fn send_delegated_join(
        &self,
        session_id: &str,
        become_active: bool,
        device_info: Value,
    ) -> Result<(), QconnectAppError> {
        let queue_version = self.queue_state_snapshot().await.version;
        let report = RendererReport::new(
            RendererReportType::RndrSrvrJoinSession,
            Uuid::new_v4().to_string(),
            queue_version,
            serde_json::json!({
                "session_uuid": session_id,
                "device_info": device_info,
                "is_active": become_active,
                "reason": JOIN_SESSION_REASON_CONTROLLER_REQUEST,
                "initial_state": {
                    "playing_state": PLAYING_STATE_STOPPED,
                    "buffer_state": RendererBufferState::Ok.as_i32(),
                    "current_position": 0,
                    "duration": 0,
                    "queue_version": {
                        "major": queue_version.major,
                        "minor": queue_version.minor,
                    }
                }
            }),
        );
        self.send_renderer_report_command(report).await
    }

    /// Emit a RndrSrvrFileAudioQualityChanged report describing the decoded file
    /// format, deduped against the last reported value. Returns Ok(true) when a
    /// report was sent. Frontend-agnostic (slice 6, step 7): both the Tauri and
    /// Slint report loops feed this; the snapshot is produced from the engine's
    /// `current_output_format()`.
    pub async fn report_file_audio_quality_if_changed(
        &self,
        queue_version: QueueVersion,
        audio_quality: QconnectFileAudioQualitySnapshot,
    ) -> Result<bool, String> {
        let sync_state = self.sync_handle();
        {
            let state = sync_state.lock().await;
            if state.last_reported_file_audio_quality == Some(audio_quality) {
                return Ok(false);
            }
        }

        let report = RendererReport::new(
            RendererReportType::RndrSrvrFileAudioQualityChanged,
            Uuid::new_v4().to_string(),
            queue_version,
            serde_json::json!({
                "sampling_rate": audio_quality.sampling_rate,
                "bit_depth": audio_quality.bit_depth,
                "nb_channels": audio_quality.nb_channels,
                "audio_quality": audio_quality.audio_quality
            }),
        );

        self.send_renderer_report_command(report)
            .await
            .map_err(|err| format!("send file audio quality report failed: {err}"))?;

        let mut state = sync_state.lock().await;
        state.last_reported_file_audio_quality = Some(audio_quality);
        Ok(true)
    }

    /// Emit a RndrSrvrDeviceAudioQualityChanged(27) report describing the actual
    /// renderer output stream format (sampling_rate / bit_depth / nb_channels), deduped against
    /// the last reported value. Returns Ok(true) when a report was sent.
    /// A nonpositive effective bit depth is unknown and omitted from the wire.
    pub async fn report_device_audio_quality_if_changed(
        &self,
        queue_version: QueueVersion,
        sampling_rate: i32,
        bit_depth: i32,
        nb_channels: i32,
    ) -> Result<bool, String> {
        let sync_state = self.sync_handle();
        let key = (sampling_rate, bit_depth, nb_channels);
        {
            let state = sync_state.lock().await;
            if state.last_reported_device_audio_quality == Some(key) {
                return Ok(false);
            }
        }

        let report = RendererReport::new(
            RendererReportType::RndrSrvrDeviceAudioQualityChanged,
            Uuid::new_v4().to_string(),
            queue_version,
            serde_json::json!({
                "sampling_rate": sampling_rate,
                "bit_depth": (bit_depth > 0).then_some(bit_depth),
                "nb_channels": nb_channels
            }),
        );

        self.send_renderer_report_command(report)
            .await
            .map_err(|err| format!("send device audio quality report failed: {err}"))?;

        let mut state = sync_state.lock().await;
        state.last_reported_device_audio_quality = Some(key);
        Ok(true)
    }

    /// Update the renderer state's position from the frontend's playback position.
    /// This keeps the internal state in sync with the actual audio playback, so that
    /// renderer reports triggered by server commands (pause/resume) include the real position.
    pub async fn update_renderer_position(&self, position_ms: u64) {
        let mut state = self.state.lock().await;
        state.renderer.current_position_ms = Some(position_ms);
    }

    pub async fn handle_transport_event(
        &self,
        event: TransportEvent,
    ) -> Result<(), QconnectAppError> {
        match event {
            TransportEvent::Connected => {
                {
                    let mut state = self.state.lock().await;
                    state.transport_connected = true;
                }
                self.sink
                    .on_event(QconnectAppEvent::TransportConnected)
                    .await;
            }
            TransportEvent::Disconnected => {
                {
                    let mut state = self.state.lock().await;
                    state.transport_connected = false;
                    state.pending.clear();
                }
                self.sink
                    .on_event(QconnectAppEvent::TransportDisconnected)
                    .await;
            }
            TransportEvent::InboundReceived(inbound) => {
                self.handle_inbound_envelope(inbound).await?;
            }
            TransportEvent::InboundQueueServerEvent(event) => {
                self.apply_server_event(event).await?;
            }
            TransportEvent::InboundRendererServerCommand(command) => {
                self.apply_renderer_server_command(command).await?;
            }
            TransportEvent::InboundPayloadBytes { payload, .. } => {
                // The batch carries renderer per-track failures as a tag-3
                // playback_error on a messages[] entry. The queue/renderer
                // decoders ignore it, so decode it here off the raw batch bytes.
                if let Some(error) = decode_playback_error(&payload) {
                    self.sink
                        .on_event(QconnectAppEvent::PlaybackError {
                            queue_item_id: error.queue_item_id,
                            error_type: error.error_type,
                            queue_version: error.queue_version,
                        })
                        .await;
                }
            }
            TransportEvent::Authenticated
            | TransportEvent::Subscribed
            | TransportEvent::SessionEstablished
            | TransportEvent::MaxReconnectAttemptsExceeded { .. }
            | TransportEvent::ReconnectScheduled { .. }
            | TransportEvent::KeepalivePingSent
            | TransportEvent::KeepalivePongReceived
            | TransportEvent::TransportError { .. }
            | TransportEvent::CloudError { .. }
            | TransportEvent::InboundFrameDecoded { .. }
            | TransportEvent::OutboundSent { .. } => {}
        }
        Ok(())
    }

    pub async fn handle_inbound_envelope(
        &self,
        inbound: InboundEnvelope,
    ) -> Result<(), QconnectAppError> {
        let event = parse_inbound_event(inbound)?;
        self.apply_server_event(event).await
    }

    async fn apply_server_event(&self, event: QueueServerEvent) -> Result<(), QconnectAppError> {
        // Session management events bypass the queue reducer entirely.
        // They provide session topology info (renderers, active renderer, etc.)
        if event.event_type.is_session_management() {
            let completed_uuid = {
                let mut state = self.state.lock().await;
                let matched_by_uuid = !state
                    .pending
                    .current()
                    .is_some_and(|p| p.is_ask_for_state_action)
                    && matches!(
                        state.pending.correlate(event.action_uuid.as_deref()),
                        PendingCorrelation::Matched
                    );
                let matched_by_session_effect = state
                    .pending
                    .current()
                    .map(|pending| {
                        session_management_event_completes_pending_action(
                            pending,
                            &event.event_type,
                            &event.payload,
                        )
                    })
                    .unwrap_or(false);
                if matched_by_uuid || matched_by_session_effect {
                    state.pending.clear().map(|pending| pending.uuid)
                } else {
                    None
                }
            };

            if let Some(uuid) = completed_uuid {
                {
                    let mut sync = self.sync.lock().await;
                    crate::confirm_local_queue_takeover_action(&mut sync, &uuid);
                }
                self.sink
                    .on_event(QconnectAppEvent::PendingActionCompleted { uuid })
                    .await;
            }

            self.sink
                .on_event(QconnectAppEvent::SessionManagementEvent {
                    message_type: event.message_type().to_string(),
                    payload: event.payload.clone(),
                })
                .await;
            return Ok(());
        }

        let mut completed_uuid: Option<String> = None;
        let mut canceled_uuid: Option<String> = None;
        let mut ignored_queue_error_uuid: Option<String> = None;
        let mut should_trigger_resync = false;
        let mut should_emit_queue_update = false;
        let remote_action_uuid = event.action_uuid.clone().unwrap_or_default();
        let snapshot: QConnectQueueState;

        {
            let mut state = self.state.lock().await;
            match state.pending.correlate(event.action_uuid.as_deref()) {
                PendingCorrelation::Matched
                    if state
                        .pending
                        .current()
                        .is_some_and(|p| p.is_ask_for_state_action)
                        && !matches!(
                            event.event_type,
                            QueueEventType::SrvrCtrlQueueState
                                | QueueEventType::SrvrCtrlQueueErrorMessage
                        ) => {}
                PendingCorrelation::Matched => {
                    completed_uuid = state.pending.clear().map(|pending| pending.uuid);
                }
                PendingCorrelation::Concurrent => {
                    if !state
                        .pending
                        .current()
                        .is_some_and(|p| p.is_ask_for_state_action)
                    {
                        state.pending.mark_concurrency_error();
                        canceled_uuid = state.pending.clear().map(|pending| pending.uuid);
                        state.concurrency_canceled_action_uuid = canceled_uuid.clone();
                        // A full snapshot already supplies authoritative recovery.
                        // Asking again here can feed an endless snapshot cycle.
                        should_trigger_resync = canceled_uuid.is_some()
                            && !matches!(event.event_type, QueueEventType::SrvrCtrlQueueState);
                    }
                    // A read is not a conflicting write. Unrelated broadcasts
                    // may be applied but must neither replace its UUID nor
                    // restart its timeout. Keep writes serialized until the
                    // correlated reply/timeout; controls can still pass above.
                }
                PendingCorrelation::EventWithoutActionUuid
                    if matches!(event.event_type, QueueEventType::SrvrCtrlQueueErrorMessage)
                        && state
                            .pending
                            .current()
                            .map(|pending| pending.is_queue_load_tracks_action)
                            .unwrap_or(false) =>
                {
                    // Queue-load rejections are emitted without the action UUID
                    // on some server paths (notably version mismatch). Keeping
                    // the slot occupied until the generic timeout prevents the
                    // authoritative local queue from being retried.
                    canceled_uuid = state.pending.clear().map(|pending| pending.uuid);
                    should_trigger_resync = canceled_uuid.is_some();
                }
                PendingCorrelation::NoPending | PendingCorrelation::EventWithoutActionUuid => {}
            }

            let ignore_queue_error =
                matches!(event.event_type, QueueEventType::SrvrCtrlQueueErrorMessage)
                    && event.action_uuid.is_some()
                    && state.concurrency_canceled_action_uuid.as_deref()
                        == event.action_uuid.as_deref();

            if ignore_queue_error {
                ignored_queue_error_uuid = event.action_uuid.clone();
                state.concurrency_canceled_action_uuid = None;
            } else {
                let queue_event = map_server_event(&event, &state.queue);
                let incomplete_shuffle = shuffle_event_needs_snapshot(&queue_event, &state.queue);
                let reducer_outcome = apply_event(&mut state.queue, &queue_event, now_ms());
                let _metric_name = telemetry::queue_reducer_event_name(reducer_outcome.event_name);
                should_emit_queue_update = true;
                if incomplete_shuffle
                    || matches!(
                        event.event_type,
                        QueueEventType::SrvrCtrlQueueTracksReordered
                            | QueueEventType::SrvrCtrlQueueTracksRemoved
                            | QueueEventType::SrvrCtrlQueueTracksAddedFromAutoplay
                    )
                {
                    should_trigger_resync = true;
                }

                // queue_hash divergence seam. Inert today (local hash algorithm
                // is a BLOCKING unknown -> compute_local_queue_hash returns None),
                // so this never fires spurious resyncs. When the algorithm lands,
                // a mismatch will pull the authoritative QueueState.
                if queue_hashes_diverge(&state.queue) {
                    should_trigger_resync = true;
                }
            }
            snapshot = state.queue.clone();
        }

        if let Some(uuid) = completed_uuid {
            {
                let mut sync = self.sync.lock().await;
                crate::confirm_local_queue_takeover_action(&mut sync, &uuid);
            }
            self.sink
                .on_event(QconnectAppEvent::PendingActionCompleted { uuid })
                .await;
        }

        if let Some(pending_uuid) = canceled_uuid {
            {
                let mut sync = self.sync.lock().await;
                crate::reject_local_queue_takeover_action(&mut sync, &pending_uuid);
            }
            self.sink
                .on_event(
                    QconnectAppEvent::PendingActionCanceledByConcurrentRemoteEvent {
                        pending_uuid,
                        remote_action_uuid,
                    },
                )
                .await;
        }

        if let Some(action_uuid) = ignored_queue_error_uuid {
            self.sink
                .on_event(QconnectAppEvent::QueueErrorIgnoredByConcurrency { action_uuid })
                .await;
        }

        if should_emit_queue_update {
            self.sink
                .on_event(QconnectAppEvent::QueueUpdated(snapshot))
                .await;
        }

        if should_trigger_resync {
            self.trigger_queue_state_resync().await;
        }
        // Serialize extraction and replay with new renderer commands. Otherwise
        // a newer pause can overtake a removed pending PLAY before its replay.
        let _execution = self.renderer_commands.lock().await;
        let deferred = {
            let mut state = self.state.lock().await;
            let ready = state.deferred_renderer_command.as_ref().is_some_and(|(_, command)| {
                let track = parse_renderer_track(&command.payload, "current_track");
                let version = command.payload.get("queue_version")
                    .and_then(|value| serde_json::from_value::<QueueVersion>(value.clone()).ok());
                version.is_none_or(|version| version.major == state.queue.version.major)
                    && track.is_none_or(|track| state.queue.queue_items.iter().chain(state.queue.autoplay_items.iter())
                        .any(|item| item.queue_item_id == track.queue_item_id && item.track_id == track.track_id))
            });
            if ready { state.deferred_renderer_command.take() } else { None }
        };
        if let Some((generation, command)) = deferred {
            if self.sync.lock().await.renderer_generation == generation {
                Box::pin(self.apply_renderer_server_command_serialized(command)).await?;
            }
        }
        Ok(())
    }

    async fn apply_renderer_server_command(
        &self,
        command: RendererServerCommand,
    ) -> Result<(), QconnectAppError> {
        let _execution = self.renderer_commands.lock().await;
        self.apply_renderer_server_command_serialized(command).await
    }

    async fn apply_renderer_server_command_serialized(
        &self,
        command: RendererServerCommand,
    ) -> Result<(), QconnectAppError> {
        let Some(mut renderer_command) = map_renderer_server_command(&command) else {
            return Ok(());
        };

        if matches!(renderer_command, RendererCommand::SetVolume { .. } | RendererCommand::MuteVolume { .. })
            && !self.sink.allows_remote_volume()
        {
            log::debug!("[QConnect] Ignoring remote volume command: local output volume is locked");
            return Ok(());
        }

        // Authority fencing belongs before the app's renderer reducer. The Qt
        // sink also guards the audio engine, but by then a stale command has
        // already mutated this snapshot and `send_renderer_reports` can echo it
        // back to the cloud, creating a feedback storm. During local takeover
        // or conflict resolution the remote renderer state has no authority at
        // either layer.
        {
            let sync = self.sync.lock().await;
            if crate::remote_renderer_commands_are_fenced(&sync) {
                log::debug!(
                    "[QConnect] Ignoring renderer command before reducer while local authority settles"
                );
                return Ok(());
            }
        }

        let generation = self.sync.lock().await.renderer_generation;
        let mut needs_queue = false;
        {
            let mut state = self.state.lock().await;
            if state.renderer_generation != generation {
                state.renderer_generation = generation;
                state.renderer.current_track = None;
                state.renderer.next_track = None;
                state.renderer.current_position_ms = None;
                state.renderer.playing_state = None;
                state.deferred_renderer_command = None;
            }
            if let RendererCommand::SetState { current_track, playing_state, .. } = &renderer_command {
                if let Some(value) = command.payload.get("queue_version").filter(|v| !v.is_null()) {
                    let Ok(version) = serde_json::from_value::<QueueVersion>(value.clone()) else {
                        return Err(QconnectAppError::RendererExecution);
                    };
                    // The official clients require major equality. Do not
                    // infer temporal ordering from an opaque queue identity.
                    needs_queue = version.major != state.queue.version.major;
                }
                if let Some(track) = current_track {
                    needs_queue |= !state.queue.queue_items.iter().chain(state.queue.autoplay_items.iter())
                        .any(|item| item.queue_item_id == track.queue_item_id && item.track_id == track.track_id);
                }
                if needs_queue {
                    // One newest intent only. A later pause/stop supersedes it;
                    // resync never resurrects a previously requested play.
                    if current_track.is_none() {
                        if let Some((pending_generation, pending)) = state.deferred_renderer_command.as_mut() {
                            if *pending_generation == generation {
                                for field in ["playing_state", "current_position", "next_track", "queue_version"] {
                                    if let Some(value) = command.payload.get(field).filter(|v| !v.is_null()) {
                                        pending.payload[field] = value.clone();
                                    }
                                }
                            }
                        } else {
                            state.deferred_renderer_command = Some((generation, command.clone()));
                        }
                    } else {
                        state.deferred_renderer_command = Some((generation, command.clone()));
                    }
                } else if current_track.is_some() || *playing_state == Some(PLAYING_STATE_STOPPED) {
                    state.deferred_renderer_command = None;
                } else if let Some((pending_generation, pending)) = state.deferred_renderer_command.as_mut() {
                    if *pending_generation == generation {
                        for field in ["playing_state", "current_position", "next_track"] {
                            if let Some(value) = command.payload.get(field).filter(|v| !v.is_null()) {
                                pending.payload[field] = value.clone();
                            }
                        }
                        // A seek for a destination still awaiting its queue
                        // must not seek the preceding audible source.
                        needs_queue = playing_state.is_none() && command.payload.get("current_position").is_some_and(|v| !v.is_null());
                    }
                }
            }
        }
        if needs_queue {
            if matches!(renderer_command, RendererCommand::SetState { playing_state: Some(PLAYING_STATE_STOPPED), .. }) {
                // STOP is independent of queue availability; do not let a
                // missing destination hold audible playback open.
                return Box::pin(self.apply_renderer_server_command_serialized(RendererServerCommand {
                    command_type: RendererCommandType::SrvrRndrSetState,
                    payload: serde_json::json!({"playing_state": PLAYING_STATE_STOPPED}),
                })).await;
            }
            if matches!(renderer_command, RendererCommand::SetState { playing_state: Some(PLAYING_STATE_PAUSED), .. }) {
                let pause = RendererCommand::SetState { playing_state: Some(PLAYING_STATE_PAUSED),
                    current_position_ms: None, current_track: None, next_track: None };
                // Pause the existing source now, then prepare the deferred
                // target once its queue arrives. No success report for that
                // unresolved target is published here.
                self.sink.execute_renderer_command(&pause, &QConnectRendererState::default()).await
                    .map_err(|_| QconnectAppError::RendererExecution)?;
            }
            self.trigger_queue_state_resync().await;
            return Ok(());
        }

        // Detect echo SET_STATE commands: the server echoes every state report
        // as a SET_STATE with only next_track (playing_state=None,
        // current_track=None, current_position_ms=None). These echoes must
        // NOT trigger CoreBridge actions (align cursor, load track,
        // resume/pause) or state reports, otherwise they destroy the local
        // queue and cause feedback loops.
        //
        // Issue #387: a peer controller (e.g. official Qobuz mobile app)
        // sends a SEEK as SET_STATE with only current_position_ms set
        // (playing_state=None, current_track=None). The previous filter
        // matched that shape and silently swallowed the command, so qbz
        // never moved its audio thread when an external controller seeked.
        // Treat current_position_ms.is_some() as the signal that the
        // command carries real intent and must be propagated.
        let is_echo = matches!(
            &renderer_command,
            RendererCommand::SetState {
                playing_state,
                current_track,
                current_position_ms,
                ..
            } if playing_state.is_none()
                && current_track.is_none()
                && current_position_ms.is_none()
        );

        let observed = self.sink.playback_event();
        let (snapshot, queue_version) = {
            let state = self.state.lock().await;
            if let Some(event) = observed.as_ref() {
                let previous_item = state
                    .renderer
                    .current_track
                    .as_ref()
                    .map(|track| track.queue_item_id);
                if crate::reporting::preserve_running_position(
                    &mut renderer_command,
                    event,
                    previous_item,
                ) {
                    log::debug!("[QConnect] Preserving live position across same-track queue echo");
                }
            }
            let mut proposed = state.renderer.clone();
            apply_renderer_command(&mut proposed, &renderer_command, now_ms());
            (proposed, state.queue.version)
        };

        if !is_echo {
            self.sink.execute_renderer_command(&renderer_command, &snapshot).await
                .map_err(|_| QconnectAppError::RendererExecution)?;
        }
        if self.sync.lock().await.renderer_generation != generation {
            return Err(QconnectAppError::RendererExecution);
        }
        // Publish only after execution is accepted. An engine failure cannot
        // become a successful renderer snapshot/report through an event sink.
        self.state.lock().await.renderer = snapshot.clone();
        self.sink.on_event(QconnectAppEvent::RendererUpdated(snapshot.clone())).await;
        if is_echo { return Ok(()); }

        self.send_renderer_reports(&renderer_command, &snapshot, queue_version)
            .await?;
        Ok(())
    }

    fn spawn_pending_timeout_watch(&self, action_uuid: String) {
        let app = self.clone();
        let handle = tokio::spawn(async move {
            app.watch_pending_action_timeout(action_uuid).await;
        });
        self.own_background_task(handle);
    }

    async fn watch_pending_action_timeout(&self, action_uuid: String) {
        tokio::time::sleep(Duration::from_millis(Self::PENDING_ACTION_TIMEOUT_MS)).await;

        let (timed_out, timed_out_ask_for_state) = {
            let mut state = self.state.lock().await;
            let (is_same_pending, is_ask_for_state_action) = state
                .pending
                .current()
                .map(|pending| {
                    (
                        pending.uuid.as_str() == action_uuid,
                        pending.is_ask_for_state_action,
                    )
                })
                .unwrap_or((false, false));
            if is_same_pending {
                state.pending.clear();
                (true, is_ask_for_state_action)
            } else {
                (false, false)
            }
        };

        if !timed_out {
            return;
        }

        {
            let mut sync = self.sync.lock().await;
            crate::reject_local_queue_takeover_action(&mut sync, &action_uuid);
        }

        self.sink
            .on_event(QconnectAppEvent::PendingActionTimedOut {
                uuid: action_uuid,
                timeout_ms: Self::PENDING_ACTION_TIMEOUT_MS,
            })
            .await;

        if !timed_out_ask_for_state {
            self.trigger_queue_state_resync().await;
        }
    }

    async fn trigger_queue_state_resync(&self) {
        let queue_version_ref = {
            let state = self.state.lock().await;
            if state.pending.current().is_some() {
                return;
            }
            state.queue.version
        };

        let command = QueueCommand::new(
            QueueCommandType::CtrlSrvrAskForQueueState,
            self.next_action_uuid(),
            queue_version_ref,
            Value::Object(Default::default()),
        );

        if self.send_queue_command(command).await.is_ok() {
            self.sink
                .on_event(QconnectAppEvent::QueueResyncTriggered)
                .await;
        }
    }

    fn next_action_uuid(&self) -> String {
        Uuid::new_v4().to_string()
    }

    async fn send_renderer_reports(
        &self,
        command: &RendererCommand,
        renderer: &QConnectRendererState,
        queue_version_ref: qconnect_core::QueueVersion,
    ) -> Result<(), QconnectAppError> {
        match command {
            RendererCommand::SetState {
                playing_state,
                current_track,
                ..
            } => {
                // Only send a state report when the SET_STATE carries a meaningful
                // change (playing_state or current_track). The server echoes every
                // state report as a SET_STATE with only next_track updated, which
                // would create an infinite feedback loop if we replied to it.
                let is_substantive = playing_state.is_some() || current_track.is_some();
                if is_substantive {
                    log::info!(
                        "[QConnect/Report] SetState report: playing={:?} pos={:?} track={:?} next={:?} qv={}.{}",
                        renderer.playing_state,
                        renderer.current_position_ms,
                        renderer.current_track.as_ref().map(|t| (t.track_id, t.queue_item_id)),
                        renderer.next_track.as_ref().map(|t| (t.track_id, t.queue_item_id)),
                        queue_version_ref.major,
                        queue_version_ref.minor
                    );
                    let observation = self.sink.playback_event();
                    let buffer_state = observation.as_ref()
                        .filter(|event| {
                            renderer
                                .current_track
                                .as_ref()
                                .is_some_and(|track| track.track_id == event.buffer_track_id || track.track_id == event.track_id)
                        })
                        .map(|event| event.buffer_state)
                        .unwrap_or_else(|| {
                            if renderer.playing_state == Some(PLAYING_STATE_PLAYING) {
                                PlaybackBufferState::InitialBuffering
                            } else {
                                PlaybackBufferState::Ready
                            }
                        });
                    let report = build_renderer_playback_report(
                        self.next_action_uuid(),
                        queue_version_ref,
                        RendererPlaybackSnapshot {
                            playing_state: renderer.playing_state.unwrap_or(PLAYING_STATE_UNKNOWN),
                            buffer_state,
                            position_ms: if matches!(command, RendererCommand::SetState { current_position_ms: None, .. }) {
                                observation.as_ref().filter(|event| renderer.current_track.as_ref()
                                    .is_some_and(|track| track.track_id == event.track_id))
                                    .map(|event| event.position.saturating_mul(1000).min(i64::MAX as u64) as i64)
                            } else { renderer.current_position_ms.map(|value| value.min(i64::MAX as u64) as i64) },
                            duration_ms: None,
                            current_queue_item_id: None,
                            next_queue_item_id: None,
                        },
                    );
                    self.send_renderer_report(report).await?;
                } else {
                    log::debug!(
                        "[QConnect/Report] Skipping echo SET_STATE report (no playing_state or current_track change)"
                    );
                }
            }
            RendererCommand::SetVolume { volume, .. } => {
                let resolved_volume = renderer.volume.or(*volume);
                if let Some(resolved_volume) = resolved_volume {
                    let report = RendererReport::new(
                        RendererReportType::RndrSrvrVolumeChanged,
                        self.next_action_uuid(),
                        queue_version_ref,
                        serde_json::json!({
                            "volume": resolved_volume
                        }),
                    );
                    self.send_renderer_report(report).await?;
                }
            }
            RendererCommand::MuteVolume { value } => {
                let report = RendererReport::new(
                    RendererReportType::RndrSrvrVolumeMuted,
                    self.next_action_uuid(),
                    queue_version_ref,
                    serde_json::json!({
                        "value": value
                    }),
                );
                self.send_renderer_report(report).await?;
            }
            RendererCommand::SetMaxAudioQuality { max_audio_quality } => {
                let report = RendererReport::new(
                    RendererReportType::RndrSrvrMaxAudioQualityChanged,
                    self.next_action_uuid(),
                    queue_version_ref,
                    serde_json::json!({
                        "max_audio_quality": max_audio_quality
                    }),
                );
                self.send_renderer_report(report).await?;
            }
            RendererCommand::SetActive { .. }
            | RendererCommand::SetLoopMode { .. }
            | RendererCommand::SetShuffleMode { .. } => {}
        }

        Ok(())
    }

    async fn send_renderer_report(&self, report: RendererReport) -> Result<(), QconnectAppError> {
        let envelope = build_qconnect_renderer_outbound_envelope(report)?;
        self.transport.send(envelope).await?;
        Ok(())
    }
}

/// BLOCKING OPEN QUESTION: the Qobuz `queue_hash` (field #100) algorithm is
/// undocumented. Until it is verified, this returns `None`, which keeps
/// `queue_hashes_diverge` inert so it can NOT fire spurious resyncs. Flipping
/// this to a real implementation is the single switch that turns on divergence
/// detection.
fn compute_local_queue_hash(_queue: &QConnectQueueState) -> Option<Vec<u8>> {
    None
}

/// Algorithm-agnostic divergence seam. Only reports divergence when BOTH a
/// locally computed hash and a server-reported hash exist and differ. Because
/// `compute_local_queue_hash` returns `None` today, this is always `false`.
fn queue_hashes_diverge(queue: &QConnectQueueState) -> bool {
    match (
        compute_local_queue_hash(queue),
        queue.last_server_queue_hash.as_ref(),
    ) {
        (Some(local), Some(server)) => &local != server,
        _ => false,
    }
}

fn map_server_event(event: &QueueServerEvent, current: &QConnectQueueState) -> QueueEvent {
    let version = event
        .queue_version
        .unwrap_or_else(|| current.version.next_minor());
    match event.event_type {
        QueueEventType::SrvrCtrlQueueState => {
            let mut next = current.clone();
            next.version = version;
            next.queue_items = parse_queue_items(&event.payload, "tracks");
            next.shuffle_mode = parse_bool(&event.payload, "shuffle_mode", current.shuffle_mode);
            next.autoplay_mode = parse_bool(&event.payload, "autoplay_mode", current.autoplay_mode);
            next.autoplay_loading =
                parse_bool(&event.payload, "autoplay_loading", current.autoplay_loading);
            next.autoplay_items = parse_queue_items(&event.payload, "autoplay_tracks");

            if next.shuffle_mode {
                let parsed_shuffle = parse_usize_list(&event.payload, "shuffled_track_indexes");
                next.shuffle_order = if !parsed_shuffle.is_empty() {
                    Some(parsed_shuffle)
                } else {
                    // A QueueState is an authoritative snapshot. Reusing an
                    // older order when the field is absent would preserve a
                    // QBZ-only permutation across a server resync.
                    None
                };
            } else {
                next.shuffle_order = None;
            }

            // Surface the server queue_hash (field #100). Only overwrite when the
            // payload actually carries it, so an omitted hash leaves prior state.
            if let Some(server_hash) = parse_bytes(&event.payload, "server_queue_hash") {
                next.last_server_queue_hash = Some(server_hash);
            }

            QueueEvent::QueueStateReplaced {
                action_uuid: event.action_uuid.clone(),
                state: next,
            }
        }
        QueueEventType::SrvrCtrlQueueTracksAdded => QueueEvent::TracksAdded {
            action_uuid: event.action_uuid.clone(),
            version,
            tracks: parse_queue_items(&event.payload, "tracks"),
            shuffle_seed: parse_u64(&event.payload, "shuffle_seed"),
            autoplay_reset: parse_bool(&event.payload, "autoplay_reset", false),
            autoplay_loading: parse_bool(&event.payload, "autoplay_loading", false),
        },
        QueueEventType::SrvrCtrlQueueTracksLoaded => QueueEvent::TracksLoaded {
            action_uuid: event.action_uuid.clone(),
            version,
            tracks: parse_queue_items(&event.payload, "tracks"),
            queue_position: parse_u64(&event.payload, "queue_position"),
            shuffle_mode: parse_optional_bool(&event.payload, "shuffle_mode"),
            shuffle_seed: parse_u64(&event.payload, "shuffle_seed"),
            shuffle_pivot_queue_item_id: parse_u64(&event.payload, "shuffle_pivot_queue_item_id"),
            autoplay_reset: parse_bool(&event.payload, "autoplay_reset", false),
            autoplay_loading: parse_bool(&event.payload, "autoplay_loading", false),
        },
        QueueEventType::SrvrCtrlQueueTracksInserted => QueueEvent::TracksInserted {
            action_uuid: event.action_uuid.clone(),
            version,
            tracks: parse_queue_items(&event.payload, "tracks"),
            insert_after: parse_u64(&event.payload, "insert_after"),
            shuffle_seed: parse_u64(&event.payload, "shuffle_seed"),
            autoplay_reset: parse_bool(&event.payload, "autoplay_reset", false),
            autoplay_loading: parse_bool(&event.payload, "autoplay_loading", false),
        },
        QueueEventType::SrvrCtrlQueueTracksRemoved => QueueEvent::TracksRemoved {
            action_uuid: event.action_uuid.clone(),
            version,
            queue_item_ids: parse_queue_item_ids(&event.payload),
            autoplay_reset: parse_bool(&event.payload, "autoplay_reset", false),
            autoplay_loading: parse_bool(&event.payload, "autoplay_loading", false),
        },
        QueueEventType::SrvrCtrlQueueTracksReordered => QueueEvent::TracksReordered {
            action_uuid: event.action_uuid.clone(),
            version,
            queue_item_ids: parse_queue_item_ids(&event.payload),
            insert_after: parse_u64(&event.payload, "insert_after"),
            autoplay_reset: parse_bool(&event.payload, "autoplay_reset", false),
            autoplay_loading: parse_bool(&event.payload, "autoplay_loading", false),
        },
        QueueEventType::SrvrCtrlQueueCleared => QueueEvent::QueueCleared {
            action_uuid: event.action_uuid.clone(),
            version,
        },
        QueueEventType::SrvrCtrlShuffleModeSet => QueueEvent::ShuffleModeSet {
            action_uuid: event.action_uuid.clone(),
            version,
            shuffle_mode: parse_bool(&event.payload, "shuffle_mode", false),
            shuffle_seed: parse_u64(&event.payload, "shuffle_seed"),
            shuffle_pivot_queue_item_id: parse_u64(&event.payload, "shuffle_pivot_queue_item_id"),
            autoplay_reset: parse_bool(&event.payload, "autoplay_reset", false),
            autoplay_loading: parse_bool(&event.payload, "autoplay_loading", false),
        },
        QueueEventType::SrvrCtrlAutoplayModeSet => QueueEvent::AutoplayModeSet {
            action_uuid: event.action_uuid.clone(),
            version,
            autoplay_mode: parse_bool(&event.payload, "autoplay_mode", false),
            autoplay_reset: parse_bool(&event.payload, "autoplay_reset", false),
            autoplay_loading: parse_bool(&event.payload, "autoplay_loading", false),
        },
        QueueEventType::SrvrCtrlAutoplayTracksLoaded => QueueEvent::AutoplayTracksLoaded {
            action_uuid: event.action_uuid.clone(),
            version,
            tracks: parse_queue_items(&event.payload, "tracks"),
        },
        QueueEventType::SrvrCtrlAutoplayTracksRemoved => QueueEvent::AutoplayTracksRemoved {
            action_uuid: event.action_uuid.clone(),
            version,
            queue_item_ids: parse_queue_item_ids(&event.payload),
        },
        QueueEventType::SrvrCtrlQueueTracksAddedFromAutoplay => QueueEvent::TracksAdded {
            action_uuid: event.action_uuid.clone(),
            version,
            tracks: Vec::new(),
            shuffle_seed: None,
            autoplay_reset: false,
            autoplay_loading: false,
        },
        QueueEventType::SrvrCtrlQueueErrorMessage => QueueEvent::QueueError {
            action_uuid: event.action_uuid.clone(),
            version: Some(version),
            code: event
                .payload
                .get("error_code")
                .map(value_to_string)
                .unwrap_or_else(|| "remote_error".to_string()),
            message: event
                .payload
                .get("error_message")
                .map(value_to_string)
                .unwrap_or_else(|| "queue_error_message".to_string()),
        },
        // Session management events are intercepted in apply_server_event()
        // and should never reach map_server_event(). Treat as no-op if they do.
        _ => QueueEvent::QueueCleared {
            action_uuid: None,
            version: current.version,
        },
    }
}

fn map_renderer_server_command(command: &RendererServerCommand) -> Option<RendererCommand> {
    match command.command_type {
        RendererCommandType::SrvrRndrSetState => Some(RendererCommand::SetState {
            playing_state: parse_i32(&command.payload, "playing_state"),
            current_position_ms: parse_u64(&command.payload, "current_position"),
            current_track: parse_renderer_track(&command.payload, "current_track"),
            next_track: parse_renderer_track(&command.payload, "next_track"),
        }),
        RendererCommandType::SrvrRndrSetVolume => Some(RendererCommand::SetVolume {
            volume: parse_i32(&command.payload, "volume"),
            volume_delta: parse_i32(&command.payload, "volume_delta"),
        }),
        RendererCommandType::SrvrRndrSetActive => Some(RendererCommand::SetActive {
            active: parse_bool(&command.payload, "active", false),
        }),
        RendererCommandType::SrvrRndrSetMaxAudioQuality => {
            parse_i32(&command.payload, "max_audio_quality")
                .map(|max_audio_quality| RendererCommand::SetMaxAudioQuality { max_audio_quality })
        }
        RendererCommandType::SrvrRndrSetLoopMode => parse_i32(&command.payload, "loop_mode")
            .map(|loop_mode| RendererCommand::SetLoopMode { loop_mode }),
        RendererCommandType::SrvrRndrSetShuffleMode => Some(RendererCommand::SetShuffleMode {
            shuffle_mode: parse_bool(&command.payload, "shuffle_mode", false),
        }),
        RendererCommandType::SrvrRndrMuteVolume => Some(RendererCommand::MuteVolume {
            value: parse_bool(&command.payload, "value", false),
        }),
    }
}

fn parse_renderer_track(payload: &Value, field: &str) -> Option<QueueItem> {
    payload.get(field).and_then(parse_queue_item)
}

fn parse_queue_items(payload: &Value, field: &str) -> Vec<QueueItem> {
    payload
        .get(field)
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_queue_item).collect())
        .unwrap_or_default()
}

fn parse_queue_item(value: &Value) -> Option<QueueItem> {
    let track_context_uuid = value
        .get("track_context_uuid")
        .or_else(|| value.get("context_uuid"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let track_id = value
        .get("track_id")
        .or_else(|| value.get("trackId"))
        .and_then(value_as_u64)?;

    let queue_item_id = value
        .get("queue_item_id")
        .or_else(|| value.get("queueItemId"))
        .and_then(value_as_u64)
        .unwrap_or(track_id);

    Some(QueueItem {
        track_context_uuid,
        track_id,
        queue_item_id,
    })
}

fn parse_queue_item_ids(payload: &Value) -> Vec<u64> {
    payload
        .get("queue_item_ids")
        .or_else(|| payload.get("queueItemsIds"))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(value_as_u64).collect())
        .unwrap_or_default()
}

fn parse_u64(payload: &Value, field: &str) -> Option<u64> {
    payload.get(field).and_then(value_as_u64)
}

/// Parse a JSON byte array (e.g. the server `queue_hash` field #100) into bytes.
/// Returns `None` when absent/null so an omitted hash does not clobber state.
fn parse_bytes(payload: &Value, field: &str) -> Option<Vec<u8>> {
    payload.get(field).and_then(Value::as_array).map(|entries| {
        entries
            .iter()
            .filter_map(value_as_u64)
            .filter_map(|value| u8::try_from(value).ok())
            .collect()
    })
}

fn parse_bool(payload: &Value, field: &str, default: bool) -> bool {
    payload
        .get(field)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn parse_optional_bool(payload: &Value, field: &str) -> Option<bool> {
    payload.get(field).and_then(Value::as_bool)
}

fn parse_i32(payload: &Value, field: &str) -> Option<i32> {
    payload.get(field).and_then(value_as_i32)
}

fn parse_usize_list(payload: &Value, field: &str) -> Vec<usize> {
    payload
        .get(field)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(value_as_u64)
                .filter_map(|value| usize::try_from(value).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn value_as_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|entry| u64::try_from(entry).ok()))
}

fn value_as_i32(value: &Value) -> Option<i32> {
    value
        .as_i64()
        .and_then(|entry| i32::try_from(entry).ok())
        .or_else(|| value.as_u64().and_then(|entry| i32::try_from(entry).ok()))
}

fn value_to_string(value: &Value) -> String {
    value
        .as_str()
        .map(ToString::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn pending_active_renderer_id_from_payload(payload: &Value) -> Option<i32> {
    payload
        .get("renderer_id")
        .or_else(|| payload.get("active_renderer_id"))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

fn session_management_event_completes_pending_action(
    pending: &PendingQueueAction,
    event_type: &QueueEventType,
    payload: &Value,
) -> bool {
    if pending.is_transport_control_action
        && matches!(event_type, QueueEventType::SrvrCtrlRendererStateUpdated)
    {
        // Transport control SET_PLAYER_STATE commands do not get a dedicated
        // action_uuid ack. The first renderer state update is the practical
        // completion signal; otherwise rapid next/previous presses stay
        // blocked behind a stale pending slot.
        return true;
    }

    if pending.is_set_loop_mode_action && matches!(event_type, QueueEventType::SrvrCtrlLoopModeSet)
    {
        // Loop mode changes come back as session-management events without a
        // stable action_uuid ack. Treat the first loop-mode-set echo as the
        // completion signal so repeat toggles are not blocked behind the
        // generic 10s pending timeout.
        return true;
    }

    if pending.is_set_volume_action && matches!(event_type, QueueEventType::SrvrCtrlVolumeChanged) {
        // Volume changes come back as session-management events without a
        // stable action_uuid ack. Treat the first volume-changed echo as the
        // completion signal so a rapid volume drag is not blocked behind the
        // generic 10s pending timeout.
        return true;
    }

    if !pending.is_set_active_renderer_action {
        return false;
    }

    let Some(expected_renderer_id) = pending.expected_active_renderer_id else {
        return false;
    };

    match event_type {
        QueueEventType::SrvrCtrlActiveRendererChanged | QueueEventType::SrvrCtrlSessionState => {
            payload
                .get("active_renderer_id")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                == Some(expected_renderer_id)
        }
        _ => false,
    }
}

/// Post-lock work returned by [`QconnectApp::apply_session_management_event`].
/// The adapter runs the CoreBridge side-effects (remote loop mode,
/// local-playback handoff, projection alignment) from these flags in order,
/// then drives the freeze + watchdog via
/// [`QconnectApp::freeze_active_renderer_projection`] /
/// [`QconnectApp::arm_renderer_watchdog`] — preserving the exact post-lock
/// ordering of the prior inline Tauri implementation.
#[derive(Debug, Default, Clone)]
pub struct SessionApplyOutcome {
    /// Active renderer whose remote projection should be re-aligned into CoreBridge.
    pub remote_projection_renderer_id: Option<i32>,
    /// Re-evaluate local playback because peer renderer state or ownership changed.
    pub sync_local_playback: bool,
    /// Apply this remote loop mode to CoreBridge.
    pub apply_loop_mode: Option<i32>,
    /// Active peer renderer left gracefully (ACTIVE_DISCONNECTED) — freeze it.
    pub disconnected_renderer_id: Option<i32>,
    /// Arm the 12s liveness watchdog for (renderer_id, generation).
    pub watchdog_arm: Option<(i32, u64)>,
}

/// Bump the watchdog epoch, invalidating any in-flight 12s liveness task.
/// Returns the new generation (captured when arming so the spawned task knows
/// which epoch it belongs to). Relocated from the Tauri sink (slice 2+4).
fn bump_watchdog_generation(state: &mut QconnectRemoteSyncState) -> u64 {
    state.watchdog_generation = state.watchdog_generation.wrapping_add(1);
    state.watchdog_generation
}

impl<TTransport, TSink> QconnectApp<TTransport, TSink>
where
    TTransport: WsTransport + 'static,
    TSink: QconnectEventSink + 'static,
{
    /// Apply a server session-management event (types 81-87, 97-101) to the
    /// shared remote-sync accumulator under ONE lock, returning the post-lock
    /// work the adapter must drive.
    ///
    /// Relocated from the Tauri event sink (slice 2+4): the locked critical
    /// section is byte-identical. The only differences are (a) the lock handle
    /// is `self.sync`, (b) local identity is injected as `identity` so the crate
    /// stays frontend-agnostic (`refresh_local_renderer_id` takes it instead of
    /// resolving device-info itself), and (c) the post-lock dispatch is RETURNED
    /// via [`SessionApplyOutcome`] rather than run inline — the CoreBridge
    /// materialization stays adapter-side because it is the renderer-engine seam
    /// (slice 6). The early `return`s (missing required fields) yield a default
    /// outcome so the adapter's post-lock block does nothing, exactly as the
    /// prior `return;` skipped it.
    pub async fn apply_session_management_event(
        &self,
        message_type: &str,
        payload: &Value,
        identity: &LocalIdentity,
    ) -> SessionApplyOutcome {
        let mut remote_projection_renderer_id: Option<i32> = None;
        let mut sync_local_playback = false;
        let mut apply_loop_mode: Option<i32> = None;
        let mut disconnected_renderer_id: Option<i32> = None;
        let mut watchdog_arm: Option<(i32, u64)> = None;
        let mut state = self.sync.lock().await;
        let previous_session = state.session.session_uuid.clone();
        let previous_active = state.session.active_renderer_id;
        match message_type {
            "MESSAGE_TYPE_SRVR_CTRL_SESSION_STATE" => {
                if let Some(uuid) = payload.get("session_uuid").and_then(Value::as_str) {
                    state.session.session_uuid = Some(uuid.to_string());
                }
                state.session.active_renderer_id = normalize_active_renderer_id(
                    payload.get("active_renderer_id").and_then(Value::as_i64),
                );
                // SESSION_STATE carries the authoritative playing state of its
                // active renderer at the top level. Cache it before the event
                // sink decides whether local playback must yield. Without this,
                // a fresh connection treated a paused peer as unknown and the
                // takeover classifier reused an unrelated renderer snapshot.
                if let (Some(active_renderer_id), Some(playing_state)) = (
                    state.session.active_renderer_id,
                    payload
                        .get("playing_state")
                        .and_then(Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok()),
                ) {
                    let renderer_state =
                        ensure_session_renderer_state(&mut state, active_renderer_id);
                    renderer_state.playing_state = Some(playing_state);
                    renderer_state.updated_at_ms = now_ms();
                }
                if let Some(loop_mode) = payload
                    .get("loop_mode")
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                {
                    if state.session_loop_mode != Some(loop_mode) {
                        state.session_loop_mode = Some(loop_mode);
                        apply_loop_mode = Some(loop_mode);
                    }
                }
                if let (Some(active_renderer_id), Some(loop_mode)) =
                    (state.session.active_renderer_id, state.session_loop_mode)
                {
                    let renderer_state =
                        ensure_session_renderer_state(&mut state, active_renderer_id);
                    renderer_state.loop_mode = Some(loop_mode);
                    renderer_state.updated_at_ms = now_ms();
                }
                sync_session_renderer_active_flags(&mut state);
                sync_local_playback = true;
            }
            "MESSAGE_TYPE_SRVR_CTRL_ADD_RENDERER" => {
                if let Some(renderer_id) = payload.get("renderer_id").and_then(Value::as_i64) {
                    let renderer_id = renderer_id as i32;
                    // Don't add duplicates
                    if !state
                        .session
                        .renderers
                        .iter()
                        .any(|r| r.renderer_id == renderer_id)
                    {
                        let device_info = payload.get("device_info");
                        state.session.renderers.push(QconnectRendererInfo {
                            renderer_id,
                            device_uuid: device_info
                                .and_then(|d| d.get("device_uuid"))
                                .and_then(Value::as_str)
                                .map(String::from),
                            friendly_name: device_info
                                .and_then(|d| d.get("friendly_name"))
                                .and_then(Value::as_str)
                                .map(String::from),
                            brand: device_info
                                .and_then(|d| d.get("brand"))
                                .and_then(Value::as_str)
                                .map(String::from),
                            model: device_info
                                .and_then(|d| d.get("model"))
                                .and_then(Value::as_str)
                                .map(String::from),
                            device_type: device_info
                                .and_then(|d| d.get("device_type"))
                                .and_then(Value::as_i64)
                                .map(|v| v as i32),
                            volume_remote_control: device_info
                                .and_then(|d| d.get("capabilities"))
                                .and_then(|c| c.get("volume_remote_control"))
                                .and_then(Value::as_i64)
                                .map(|v| v as i32),
                        });
                        refresh_local_renderer_id(&mut state.session, identity);
                    }
                    let _ = ensure_session_renderer_state(&mut state, renderer_id);
                    sync_session_renderer_active_flags(&mut state);
                }
            }
            "MESSAGE_TYPE_SRVR_CTRL_UPDATE_RENDERER" => {
                if let Some(renderer_id) = payload.get("renderer_id").and_then(Value::as_i64) {
                    let renderer_id = renderer_id as i32;
                    if let Some(existing) = state
                        .session
                        .renderers
                        .iter_mut()
                        .find(|r| r.renderer_id == renderer_id)
                    {
                        let device_info = payload.get("device_info");
                        if let Some(device_uuid) = device_info
                            .and_then(|d| d.get("device_uuid"))
                            .and_then(Value::as_str)
                        {
                            existing.device_uuid = Some(device_uuid.to_string());
                        }
                        if let Some(name) = device_info
                            .and_then(|d| d.get("friendly_name"))
                            .and_then(Value::as_str)
                        {
                            existing.friendly_name = Some(name.to_string());
                        }
                        if let Some(brand) = device_info
                            .and_then(|d| d.get("brand"))
                            .and_then(Value::as_str)
                        {
                            existing.brand = Some(brand.to_string());
                        }
                        if let Some(model) = device_info
                            .and_then(|d| d.get("model"))
                            .and_then(Value::as_str)
                        {
                            existing.model = Some(model.to_string());
                        }
                        if let Some(device_type) = device_info
                            .and_then(|d| d.get("device_type"))
                            .and_then(Value::as_i64)
                        {
                            existing.device_type = Some(device_type as i32);
                        }
                        if let Some(vrc) = device_info
                            .and_then(|d| d.get("capabilities"))
                            .and_then(|c| c.get("volume_remote_control"))
                            .and_then(Value::as_i64)
                        {
                            existing.volume_remote_control = Some(vrc as i32);
                        }
                        refresh_local_renderer_id(&mut state.session, identity);
                    }
                    let _ = ensure_session_renderer_state(&mut state, renderer_id);
                    sync_session_renderer_active_flags(&mut state);
                }
            }
            "MESSAGE_TYPE_SRVR_CTRL_REMOVE_RENDERER" => {
                if let Some(renderer_id) = payload.get("renderer_id").and_then(Value::as_i64) {
                    let renderer_id = renderer_id as i32;
                    state
                        .session
                        .renderers
                        .retain(|r| r.renderer_id != renderer_id);
                    state.session_renderer_states.remove(&renderer_id);
                    refresh_local_renderer_id(&mut state.session, identity);
                    sync_session_renderer_active_flags(&mut state);
                }
            }
            "MESSAGE_TYPE_SRVR_CTRL_ACTIVE_RENDERER_CHANGED" => {
                let local_was_active = is_local_renderer_active(&state.session);
                state.session.active_renderer_id = normalize_active_renderer_id(
                    payload.get("active_renderer_id").and_then(Value::as_i64),
                );
                if local_was_active && is_peer_renderer_active(&state.session) {
                    // A real local -> peer handoff supersedes any unfinished
                    // local queue publication. Never carry its ids/action uuid
                    // into a later takeback in the same runtime.
                    crate::clear_local_queue_takeover(&mut state);
                    crate::set_local_playback_conflict_pending(&mut state, false);
                }
                if let (Some(active_renderer_id), Some(loop_mode)) =
                    (state.session.active_renderer_id, state.session_loop_mode)
                {
                    let renderer_state =
                        ensure_session_renderer_state(&mut state, active_renderer_id);
                    renderer_state.loop_mode = Some(loop_mode);
                    renderer_state.updated_at_ms = now_ms();
                }
                apply_loop_mode = state.session_loop_mode;
                sync_session_renderer_active_flags(&mut state);
                remote_projection_renderer_id = state.session.active_renderer_id;
                sync_local_playback = true;
                // P0-1: active-renderer change disarms any pending liveness task.
                bump_watchdog_generation(&mut state);
            }
            "MESSAGE_TYPE_SRVR_CTRL_RENDERER_STATE_UPDATED" => {
                let Some(renderer_id) = payload.get("renderer_id").and_then(Value::as_i64) else {
                    return SessionApplyOutcome::default();
                };
                let player_state = payload.get("player_state");
                let renderer_state = ensure_session_renderer_state(&mut state, renderer_id as i32);

                if let Some(playing_state) = player_state
                    .and_then(|value| value.get("playing_state"))
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                {
                    renderer_state.playing_state = Some(playing_state);
                }

                if let Some(current_position_ms) = player_state
                    .and_then(|value| value.get("current_position"))
                    .and_then(Value::as_i64)
                    .and_then(|value| u64::try_from(value).ok())
                {
                    renderer_state.current_position_ms = Some(current_position_ms);
                }

                if let Some(current_queue_item_id) = player_state
                    .and_then(|value| value.get("current_queue_item_id"))
                    .and_then(Value::as_i64)
                {
                    renderer_state.current_queue_item_id =
                        u64::try_from(current_queue_item_id).ok();
                }

                renderer_state.updated_at_ms = now_ms();
                // Snapshot the just-written playing_state for the watchdog decision
                // (the &mut borrow ends here so we can re-read state.session below).
                let this_playing_state = renderer_state.playing_state;
                remote_projection_renderer_id = Some(renderer_id as i32);
                sync_local_playback = true;

                let is_active_peer = state.session.active_renderer_id == Some(renderer_id as i32)
                    && is_peer_renderer_active(&state.session)
                    && renderer_id != -1;

                // P0-2: graceful disconnect. status==ACTIVE_DISCONNECTED(2) for
                // the active remote renderer (id != -1) means the renderer left
                // cleanly — freeze the projection so the UI stops lying.
                let status =
                    RendererStatus::from_wire(payload.get("status").and_then(Value::as_i64));
                if status == RendererStatus::ActiveDisconnected && is_active_peer {
                    disconnected_renderer_id = Some(renderer_id as i32);
                }

                // P0-1 liveness watchdog. Any RENDERER_STATE_UPDATED for the
                // active peer resets the timer (bump generation); arm a fresh 12s
                // task only while PLAYING. A non-playing update disarms (bump
                // only, no spawn).
                let new_generation = bump_watchdog_generation(&mut state);
                if should_arm_renderer_watchdog(this_playing_state, is_active_peer) {
                    watchdog_arm = Some((renderer_id as i32, new_generation));
                }
            }
            "MESSAGE_TYPE_SRVR_CTRL_VOLUME_CHANGED" => {
                let Some(renderer_id) = payload.get("renderer_id").and_then(Value::as_i64) else {
                    return SessionApplyOutcome::default();
                };
                let Some(volume) = payload
                    .get("volume")
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                else {
                    return SessionApplyOutcome::default();
                };

                let renderer_state = ensure_session_renderer_state(&mut state, renderer_id as i32);
                renderer_state.volume = Some(volume);
                renderer_state.updated_at_ms = now_ms();
            }
            "MESSAGE_TYPE_SRVR_CTRL_VOLUME_MUTED" => {
                let Some(renderer_id) = payload.get("renderer_id").and_then(Value::as_i64) else {
                    return SessionApplyOutcome::default();
                };
                let Some(muted) = payload.get("value").and_then(Value::as_bool) else {
                    return SessionApplyOutcome::default();
                };

                let renderer_state = ensure_session_renderer_state(&mut state, renderer_id as i32);
                renderer_state.muted = Some(muted);
                renderer_state.updated_at_ms = now_ms();
            }
            "MESSAGE_TYPE_SRVR_CTRL_MAX_AUDIO_QUALITY_CHANGED" => {
                let Some(renderer_id) = payload.get("renderer_id").and_then(Value::as_i64) else {
                    return SessionApplyOutcome::default();
                };
                let Some(max_audio_quality) = payload
                    .get("max_audio_quality")
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                else {
                    return SessionApplyOutcome::default();
                };

                let renderer_state = ensure_session_renderer_state(&mut state, renderer_id as i32);
                renderer_state.max_audio_quality = Some(max_audio_quality);
                renderer_state.updated_at_ms = now_ms();
            }
            "MESSAGE_TYPE_SRVR_CTRL_LOOP_MODE_SET" => {
                let Some(loop_mode) = payload
                    .get("loop_mode")
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                else {
                    return SessionApplyOutcome::default();
                };
                if state.session_loop_mode != Some(loop_mode) {
                    state.session_loop_mode = Some(loop_mode);
                    apply_loop_mode = Some(loop_mode);
                }
                if let Some(active_renderer_id) = state.session.active_renderer_id {
                    let renderer_state =
                        ensure_session_renderer_state(&mut state, active_renderer_id);
                    renderer_state.loop_mode = Some(loop_mode);
                    renderer_state.updated_at_ms = now_ms();
                }
            }
            _ => {}
        }

        if (previous_session.is_some() && previous_session != state.session.session_uuid)
            || (previous_active != state.session.active_renderer_id && is_peer_renderer_active(&state.session))
        {
            state.renderer_generation = state.renderer_generation.wrapping_add(1);
            state.last_load_attempt = None;
        }

        SessionApplyOutcome {
            remote_projection_renderer_id,
            sync_local_playback,
            apply_loop_mode,
            disconnected_renderer_id,
            watchdog_arm,
        }
    }

    /// Arm the generation-guarded 12s renderer-liveness watchdog (P0-1). Spawns
    /// a task holding a cheap clone of self. On wake it re-locks the sync
    /// accumulator and no-ops unless its captured epoch is still current AND the
    /// renderer is still the active peer AND still nominally PLAYING; only then
    /// does it freeze the projection and emit `RendererUnreachable`. Modeled on
    /// `watch_pending_action_timeout`; relocated from the Tauri sink (slice 2+4).
    pub fn arm_renderer_watchdog(&self, renderer_id: i32, generation: u64) {
        let app = self.clone();
        let handle = tokio::spawn(async move {
            app.run_renderer_watchdog(renderer_id, generation).await;
        });
        self.own_background_task(handle);
    }

    fn own_background_task(&self, handle: tokio::task::JoinHandle<()>) {
        let mut tasks = self
            .background_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !tasks.accepting {
            handle.abort();
            return;
        }
        tasks.handles.retain(|task| !task.is_finished());
        tasks.handles.push(handle);
    }

    fn set_background_tasks_accepting(&self, accepting: bool) {
        let mut tasks = self
            .background_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        tasks.accepting = accepting;
    }

    async fn abort_background_tasks(&self) {
        let handles = {
            let mut tasks = self
                .background_tasks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            tasks.accepting = false;
            std::mem::take(&mut tasks.handles)
        };
        for handle in &handles {
            handle.abort();
        }
        for handle in handles {
            let _ = handle.await;
        }
    }

    async fn run_renderer_watchdog(&self, renderer_id: i32, generation: u64) {
        tokio::time::sleep(Duration::from_millis(QCONNECT_RENDERER_LOST_TIMEOUT_MS)).await;
        let fire = {
            let state = self.sync.lock().await;
            state.watchdog_generation == generation
                && state.session.active_renderer_id == Some(renderer_id)
                && is_peer_renderer_active(&state.session)
                && state
                    .session_renderer_states
                    .get(&renderer_id)
                    .and_then(|r| r.playing_state)
                    == Some(PLAYING_STATE_PLAYING)
        };
        if fire {
            log::warn!(
                "[QConnect] Renderer {renderer_id} silent for {}ms — marking unreachable",
                QCONNECT_RENDERER_LOST_TIMEOUT_MS
            );
            self.freeze_active_renderer_projection(
                renderer_id,
                QconnectAppEvent::RendererUnreachable { renderer_id },
            )
            .await;
        }
    }

    /// Force the active renderer's cached projection to UNKNOWN/stopped and emit
    /// the freeze event through the sink. Shared by ACTIVE_DISCONNECTED (P0-2)
    /// and the silence watchdog (P0-1). Relocated from the Tauri sink (slice
    /// 2+4): the state mutation + `self.sink.on_event(freeze_event)` is
    /// byte-identical. The sink's mapper arm for `RendererUnreachable` /
    /// `RendererDisconnected` ONLY emits the dedicated channel (+ the blanket
    /// event); it does NOT route back into apply or call this helper again, so it
    /// cannot re-arm the watchdog or recurse.
    pub async fn freeze_active_renderer_projection(
        &self,
        renderer_id: i32,
        freeze_event: QconnectAppEvent,
    ) {
        {
            let mut state = self.sync.lock().await;
            let renderer_state = ensure_session_renderer_state(&mut state, renderer_id);
            renderer_state.playing_state = Some(PLAYING_STATE_UNKNOWN);
            renderer_state.updated_at_ms = now_ms();
        }
        self.sink.on_event(freeze_event).await;
    }
}

/// Prior-state snapshot captured from the sync accumulator BEFORE a SESSION_STATE
/// frame is applied, plus the server's classified active state derived from the
/// incoming payload. Drives P1-3 takeover arbitration after the apply. Relocated
/// from the Tauri adapter (slice 5) so the shared session loop owns it.
#[derive(Debug, Clone, Copy)]
pub struct SessionStateTakeoverInput {
    pub was_active: bool,
    pub was_playing: bool,
    pub server: ServerActiveState,
    pub active_renderer_id: Option<i32>,
}

/// Explicit user decision when local audio and a peer renderer are both live
/// during QConnect bootstrap. The numeric ordering belongs to the Qt modal;
/// this enum keeps the control flow frontend-independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalPlaybackConflictChoice {
    ContinueOnActiveRenderer,
    ContinueOnThisDevice,
    ContinueLocalPlaybackAndReplaceQueue,
    CancelConnection,
}

/// Preview the first 8 `track_id`s under `payload[key]` for diagnostic emits.
/// Pure; relocated from the Tauri adapter (slice 5).
pub fn queue_payload_track_preview(payload: &Value, key: &str) -> Vec<i64> {
    payload
        .get(key)
        .and_then(Value::as_array)
        .map(|tracks| {
            tracks
                .iter()
                .filter_map(|track| track.get("track_id").and_then(Value::as_i64))
                .take(8)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

impl<TTransport, TSink> QconnectApp<TTransport, TSink>
where
    TTransport: WsTransport + 'static,
    TSink: QconnectEventSink + 'static,
{
    /// Capture prior renderer ownership and classify the incoming server state.
    /// `local_playback_is_playing` comes from the adapter's real player, not a
    /// remote-renderer cache; that distinction is the local-playing takeover
    /// rule's source of truth.
    pub async fn capture_session_state_takeover_input(
        &self,
        payload: &Value,
        local_playback_is_playing: bool,
    ) -> SessionStateTakeoverInput {
        let st = self.sync.lock().await;
        let local_id = st.session.local_renderer_id;
        let was_active = is_local_renderer_active(&st.session);
        let was_playing = local_playback_is_playing;
        // Normalize identically to the apply path (`normalize_active_renderer_id`):
        // the cloud encodes "no active renderer" as `active_renderer_id: -1`, so a
        // raw parse would classify -1 as `Some(-1)` and fall into the `Some(_)`
        // peer-active arm — suppressing the idle auto-take. Filtering `>= 0` maps
        // -1 (and any negative sentinel) to None, the genuine "session idle" state.
        let incoming_active =
            normalize_active_renderer_id(payload.get("active_renderer_id").and_then(Value::as_i64));
        let incoming_playing_state = payload
            .get("playing_state")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .or_else(|| {
                incoming_active.and_then(|renderer_id| {
                    st.session_renderer_states
                        .get(&renderer_id)
                        .and_then(|renderer| renderer.playing_state)
                })
            });
        let server = match incoming_active {
            None => ServerActiveState::None,
            Some(id) if Some(id) == local_id => ServerActiveState::Me,
            Some(_) => {
                // The incoming active renderer's own state is authoritative.
                // Unknown is conservative for continuity: it is not evidence
                // that another renderer is actually playing.
                if incoming_playing_state == Some(PLAYING_STATE_PLAYING) {
                    ServerActiveState::OtherPlaying
                } else {
                    ServerActiveState::OtherPaused
                }
            }
        };
        SessionStateTakeoverInput {
            was_active,
            was_playing,
            server,
            active_renderer_id: incoming_active,
        }
    }

    /// Runtime-only authority fences must not survive an explicit cancel or a
    /// failed conflict resolution into the next connection attempt.
    async fn clear_local_playback_authority_fences(&self) {
        let mut sync = self.sync.lock().await;
        crate::clear_local_queue_takeover(&mut sync);
        crate::set_local_playback_conflict_pending(&mut sync, false);
    }

    /// Clear the pending action slot iff it still matches `action_uuid`. Used for
    /// replies that are not reducer-correlated (set-active, ask-for-state). Locks
    /// `self.state` (NEVER co-locked with `self.sync`). Relocated from the Tauri
    /// adapter (slice 5).
    pub async fn clear_pending_if_matches(&self, action_uuid: &str) {
        let mut state = self.state.lock().await;
        let pending_matches = state
            .pending
            .current()
            .map(|pending| pending.uuid == action_uuid)
            .unwrap_or(false);
        if pending_matches {
            state.pending.clear();
        }
    }

    /// Guarded `CtrlSrvrSetActiveRenderer` sender. No-ops when the target equals
    /// the current active renderer or the id is invalid. Never resets reconnect
    /// counters (#358 latch untouched). Relocated from the Tauri adapter's
    /// `send_set_active_renderer_via_app` (slice 5); the request payload
    /// `{ "renderer_id": <i32> }` is byte-identical to the prior
    /// `QconnectSetActiveRendererRequest` serialization.
    pub async fn send_set_active_renderer(&self, target_renderer_id: i32) -> Result<bool, String> {
        if target_renderer_id < 0 {
            return Ok(false);
        }
        {
            let st = self.sync.lock().await;
            if st.session.active_renderer_id == Some(target_renderer_id) {
                return Ok(false);
            }
        }
        {
            let mut state = self.state.lock().await;
            let superseded_transport = state
                .pending
                .current()
                .map(|pending| pending.is_transport_control_action)
                .unwrap_or(false);
            if superseded_transport {
                state.pending.clear();
            }
        }
        let payload = serde_json::json!({ "renderer_id": target_renderer_id });
        let command = self
            .build_queue_command(QueueCommandType::CtrlSrvrSetActiveRenderer, payload)
            .await;
        let uuid = self
            .send_queue_command(command)
            .await
            .map_err(|err| format!("send set_active_renderer failed: {err}"))?;
        self.clear_pending_if_matches(&uuid).await;
        Ok(true)
    }

    /// Ask the currently-active renderer for its full state (P1-8). No-op when no
    /// active renderer is known. The reply is not reducer-correlated, so its
    /// pending slot is cleared immediately. Relocated from the Tauri adapter
    /// (slice 5); payload byte-identical to the prior
    /// `QconnectAskForRendererStateRequest` serialization.
    pub async fn ask_for_active_renderer_state(&self) -> Result<bool, String> {
        let active_id = {
            let st = self.sync.lock().await;
            st.session.active_renderer_id
        };
        let Some(active_id) = active_id else {
            return Ok(false);
        };
        let payload = serde_json::json!({ "renderer_id": active_id });
        let cmd = self
            .build_queue_command(QueueCommandType::CtrlSrvrAskForRendererState, payload)
            .await;
        let uuid = self
            .send_queue_command(cmd)
            .await
            .map_err(|err| format!("send ask_for_renderer_state failed: {err}"))?;
        self.clear_pending_if_matches(&uuid).await;
        Ok(true)
    }

    /// Ask the server for the current queue state (P1-8 Lagged-recovery / generic
    /// re-sync). The reply is not reducer-correlated, so its pending slot is
    /// cleared. Relocated from the Tauri adapter (slice 5).
    pub async fn ask_for_queue_state(&self) -> Result<(), String> {
        let cmd = self
            .build_queue_command(
                QueueCommandType::CtrlSrvrAskForQueueState,
                serde_json::json!({}),
            )
            .await;
        let uuid = self
            .send_queue_command(cmd)
            .await
            .map_err(|err| format!("send ask_for_queue_state failed: {err}"))?;
        self.clear_pending_if_matches(&uuid).await;
        Ok(())
    }
}

/// A complete WS shuffle event is itself authoritative (Mac 8.2 controller
/// onShuffleModeSet); materializing its seed is not grounds for another read.
/// Still recover when the input cannot describe the order of our known queue.
fn shuffle_event_needs_snapshot(event: &QueueEvent, queue: &QConnectQueueState) -> bool {
    let QueueEvent::ShuffleModeSet {
        shuffle_mode,
        shuffle_seed,
        shuffle_pivot_queue_item_id,
        ..
    } = event
    else {
        return false;
    };
    *shuffle_mode
        && (shuffle_seed.is_none()
            || shuffle_pivot_queue_item_id.is_some_and(|pivot| {
                pivot != 0
                    && !queue
                        .queue_items
                        .iter()
                        .any(|item| item.queue_item_id == pivot)
            }))
}

/// Attempt budget for the Lagged-recovery re-AskForQueueState loop (P1-8).
const QCONNECT_REASK_QUEUE_STATE_MAX_ATTEMPTS: u32 = 5;

/// Adapter-provided seams for the shared session loop. `run_session_loop` is
/// frontend-agnostic; it calls these for the bits that must stay adapter-side:
/// the lifecycle gating (depends on the adapter's runtime ownership), the
/// reconnect-exhausted teardown (R3), controller bootstrap, the renderer-join
/// reports (which read the engine's track duration — R2), and the raw error
/// surface. Both the Tauri adapter and a future Slint adapter implement this.
#[async_trait]
pub trait SessionLoopHost: Send + Sync {
    /// Synchronous snapshot of the adapter's actual local player.
    fn local_playback_is_playing(&self) -> bool {
        false
    }
    /// Publish the resolvable projection of the local queue during a
    /// local-playing takeover. Returns true only when a non-empty queue command
    /// was accepted by the transport.
    async fn publish_local_queue_for_takeover(&self) -> bool {
        false
    }
    /// Ask the frontend which side wins when a peer renderer and local audio
    /// are both active during bootstrap. Headless adapters retain the previous
    /// server-authoritative behavior by default.
    async fn resolve_local_playback_conflict(
        &self,
        _active_renderer_id: i32,
        peer_was_playing: bool,
    ) -> LocalPlaybackConflictChoice {
        if peer_was_playing {
            LocalPlaybackConflictChoice::ContinueOnActiveRenderer
        } else {
            // Preserve the pre-modal headless policy: a paused/stale peer does
            // not interrupt real local playback.
            LocalPlaybackConflictChoice::ContinueLocalPlaybackAndReplaceQueue
        }
    }
    /// Stop local audio before deliberately adopting the remote queue.
    async fn stop_local_playback_for_remote_queue(&self) {}
    /// Resume a paused active peer when option 1 explicitly asks to continue it.
    async fn continue_active_renderer_playback(&self) {}
    /// Tear down QConnect without applying the remote queue or disturbing the
    /// current local queue/playback.
    async fn cancel_connection_for_playback_conflict(&self) {}
    /// Surface a lifecycle transition. The Tauri adapter gates + dedups this
    /// (emits `qconnect:status_changed` via the sink only while a runtime is
    /// still alive); a Slint adapter does its own.
    async fn update_lifecycle(&self, state: QconnectLifecycleState);
    /// Re-bootstrap controller presence after a reconnect (JoinSession + ask
    /// queue state). Logs its own errors.
    async fn bootstrap_after_reconnect(&self);
    /// Renderer JoinSession + initial reports for `session_uuid` (R2: reads the
    /// current track duration from the adapter's engine). Idempotent per uuid.
    async fn deferred_renderer_join(&self, session_uuid: String, reason: i32);
    /// Handle MaxReconnectAttemptsExceeded (R3): set Exhausted + last_error, drop
    /// the runtime unless idle-retry is active, surface the terminal status.
    /// Returns true if the loop should break (terminate), false to keep idling.
    async fn on_reconnect_exhausted(
        &self,
        attempts: u32,
        last_reason: String,
        idle_retry_active: bool,
    ) -> bool;
    /// Surface a transport-handling error (the raw `qconnect:error` channel on Tauri).
    async fn on_loop_error(&self, message: String);
}

impl<TTransport, TSink> QconnectApp<TTransport, TSink>
where
    TTransport: WsTransport + 'static,
    TSink: QconnectEventSink + 'static,
{
    /// The shared transport event loop. Relocated from the Tauri adapter (slice
    /// 5): consumes transport events and drives takeover arbitration / deferred
    /// renderer join / reconnect resync / Lagged re-ask, routing diagnostics +
    /// lifecycle through `self.sink` + the injected `host`. Byte-identical to the
    /// prior inline Tauri loop; the adapter-coupled seams (lifecycle gating,
    /// teardown, bootstrap, renderer-join, raw error) go through `host` so both
    /// frontends share this control flow.
    pub async fn run_session_loop(
        &self,
        host: Arc<dyn SessionLoopHost>,
        transport_rx: tokio::sync::broadcast::Receiver<TransportEvent>,
        idle_retry_active: bool,
    ) {
        self.run_session_loop_with_prefetched(
            host,
            transport_rx,
            idle_retry_active,
            VecDeque::new(),
        )
        .await;
    }

    /// Run the shared transport loop after draining events that this exact
    /// receiver already consumed during a transactional preflight.
    ///
    /// `prefetched` is processed once, in FIFO order, before the receiver is
    /// polled again. Callers must move (not copy) consumed events into this
    /// queue; the receiver then resumes at its existing cursor, so the handoff
    /// has neither a loss window nor a replay duplicate.
    pub async fn run_session_loop_with_prefetched(
        &self,
        host: Arc<dyn SessionLoopHost>,
        mut transport_rx: tokio::sync::broadcast::Receiver<TransportEvent>,
        idle_retry_active: bool,
        mut prefetched: VecDeque<TransportEvent>,
    ) {
        // NOTE: `transport_rx` is subscribed by the adapter SYNCHRONOUSLY before
        // this task is spawned (and before any further await), so the receiver is
        // live before the WS handshake can emit its initial events. tokio
        // broadcast has no replay — subscribing here, inside the spawned task,
        // would race those events and silently drop them.
        log::info!("[QConnect/EventLoop] Started listening for transport events");
        {
            // A previous runtime may have ended through the modal's Cancel
            // path before its queued Disconnected event was observed. Runtime
            // fences never carry authority into a new connection. If real
            // local audio is already playing, however, fence bootstrap traffic
            // until SESSION_STATE + the authoritative QueueState establish
            // which side may mutate it.
            let mut sync = self.sync.lock().await;
            crate::clear_local_queue_takeover(&mut sync);
            crate::set_local_playback_conflict_pending(&mut sync, host.local_playback_is_playing());
        }
        let mut authoritative_queue_state_received = false;
        let mut renderer_joined = false;
        let mut has_disconnected = false;
        // One-shot latch: auto-take the render at most ONCE per fresh connect, on
        // the first SESSION_STATE that reports no active renderer. Prevents
        // re-grabbing after the user later gives the render to a peer (which makes
        // the session go idle again). Reset on disconnect (a reconnect rejoins via
        // the RECONNECTION reason, not via auto-take).
        let mut auto_take_attempted = false;
        // Set when we DECIDED to auto-take on a no-active SESSION_STATE but our
        // local_renderer_id was not known yet (the cloud's ADD_RENDERER for our
        // just-sent join lands a beat later). The per-iteration check below fires
        // the SET_ACTIVE as soon as the id arrives — and ONLY if nobody else has
        // become active meanwhile (anti-steal). Reset on disconnect.
        let mut pending_auto_take = false;
        // A local-playing takeover may be decided before the renderer ADD echo
        // gives us our local id. Keep that decision until the id arrives, but
        // cancel it immediately if local playback stops or the active peer
        // reports PLAYING in the meantime.
        let mut pending_local_takeover = false;
        let mut pending_local_takeover_is_explicit = false;
        // Option 2 can be chosen before ADD_RENDERER assigns our local id.
        // Keep remote-queue authority fenced until that id arrives and we can
        // claim this device intentionally.
        let mut pending_remote_queue_takeover = false;
        // One explicit choice per runtime. Reconnect frames in the same runtime
        // must not reopen the modal after the user already chose an authority.
        let mut playback_conflict_resolved = false;
        // P1-8: budget for re-AskForQueueState after Lagged broadcast drops,
        // until the session_uuid is confirmed. Reset on disconnect.
        let mut lagged_reask_attempts: u32 = 0;
        loop {
            let next_event = match prefetched.pop_front() {
                Some(event) => Ok(event),
                None => transport_rx.recv().await,
            };
            match next_event {
                Ok(event) => {
                    let received_authoritative_queue_state = matches!(
                        &event,
                        TransportEvent::InboundQueueServerEvent(event)
                            if event.event_type == QueueEventType::SrvrCtrlQueueState
                    );
                    // P1-3: on every SESSION_STATE, capture our prior active/playing
                    // state + classify the server's active renderer BEFORE the sink
                    // applies the new state, so takeover arbitration runs on the
                    // prior snapshot once the apply lands below.
                    let mut takeover_input: Option<SessionStateTakeoverInput> = None;
                    if let TransportEvent::InboundQueueServerEvent(ref evt) = event {
                        if evt.message_type() == "MESSAGE_TYPE_SRVR_CTRL_SESSION_STATE" {
                            let input = self
                                .capture_session_state_takeover_input(
                                    &evt.payload,
                                    host.local_playback_is_playing(),
                                )
                                .await;
                            if !playback_conflict_resolved
                                && input.was_playing
                                && matches!(
                                    input.server,
                                    ServerActiveState::OtherPlaying
                                        | ServerActiveState::OtherPaused
                                )
                            {
                                let mut sync = self.sync.lock().await;
                                crate::set_local_playback_conflict_pending(&mut sync, true);
                            }
                            takeover_input = Some(input);
                            if !renderer_joined {
                                if let Some(session_uuid) =
                                    evt.payload.get("session_uuid").and_then(|v| v.as_str())
                                {
                                    renderer_joined = true;
                                    host.deferred_renderer_join(
                                        session_uuid.to_string(),
                                        deferred_join_reason(has_disconnected),
                                    )
                                    .await;
                                } else {
                                    log::warn!(
                                        "[QConnect] SESSION_STATE received without session_uuid"
                                    );
                                }
                            }
                        }
                    }
                    match &event {
                        TransportEvent::Connected => {
                            log::info!("[QConnect/Transport] WebSocket connected");
                        }
                        TransportEvent::Disconnected => {
                            log::warn!("[QConnect/Transport] WebSocket disconnected — resetting renderer_joined flag");
                            renderer_joined = false;
                            has_disconnected = true;
                            auto_take_attempted = false;
                            pending_auto_take = false;
                            pending_local_takeover = false;
                            pending_local_takeover_is_explicit = false;
                            pending_remote_queue_takeover = false;
                            authoritative_queue_state_received = false;
                            lagged_reask_attempts = 0;
                            {
                                let mut sync = self.sync.lock().await;
                                crate::clear_local_queue_takeover(&mut sync);
                                crate::set_local_playback_conflict_pending(
                                    &mut sync,
                                    host.local_playback_is_playing(),
                                );
                            }
                            // Surface "Reconnecting" to the UI, but only if we're
                            // not in a teardown path (Off/Exhausted). The host gates
                            // this on a live runtime.
                            host.update_lifecycle(QconnectLifecycleState::Reconnecting)
                                .await;
                        }
                        TransportEvent::Authenticated => {
                            log::info!("[QConnect/Transport] Authenticated with JWT");
                        }
                        TransportEvent::Subscribed => {
                            log::info!("[QConnect/Transport] Subscribed to channels");
                            // Re-bootstrap only after a reconnection (not on initial
                            // connect, where connect() already bootstraps).
                            if has_disconnected {
                                log::info!("[QConnect] Re-bootstrapping after reconnect...");
                                host.bootstrap_after_reconnect().await;
                                // P1-8: resync renderer state too, not just the queue.
                                if let Err(err) = self.ask_for_active_renderer_state().await {
                                    log::warn!("[QConnect] AskForRendererState after reconnect failed: {err}");
                                }
                                // P1-11: signal the frontend that the post-reconnect
                                // resync has been issued.
                                self.sink.on_event(QconnectAppEvent::ResyncComplete).await;
                            }
                        }
                        TransportEvent::KeepalivePingSent => {
                            log::debug!("[QConnect/Transport] Keepalive ping sent");
                        }
                        TransportEvent::KeepalivePongReceived => {
                            log::debug!("[QConnect/Transport] Keepalive pong received");
                        }
                        TransportEvent::ReconnectScheduled {
                            attempt,
                            backoff_ms,
                            reason,
                        } => {
                            log::warn!("[QConnect/Transport] Reconnect scheduled: attempt={} backoff={}ms reason={}", attempt, backoff_ms, reason);
                        }
                        TransportEvent::InboundQueueServerEvent(evt) => {
                            log::debug!(
                                "[QConnect] <-- Inbound queue event: {} tracks={} autoplay_tracks={}",
                                evt.message_type(),
                                evt.payload.get("tracks").and_then(|value| value.as_array()).map_or(0, Vec::len),
                                evt.payload.get("autoplay_tracks").and_then(|value| value.as_array()).map_or(0, Vec::len),
                            );
                            self.sink
                                .on_event(QconnectAppEvent::Diagnostic {
                                    channel: "qconnect:inbound_queue_event".to_string(),
                                    level: "info".to_string(),
                                    payload: serde_json::json!({
                                        "message_type": evt.message_type(),
                                        "action_uuid": evt.action_uuid.clone(),
                                        "queue_version": evt.queue_version,
                                        "track_count": evt.payload.get("tracks").and_then(|value| value.as_array()).map(|tracks| tracks.len()).unwrap_or(0),
                                        "autoplay_track_count": evt.payload.get("autoplay_tracks").and_then(|value| value.as_array()).map(|tracks| tracks.len()).unwrap_or(0),
                                        "preview_track_ids": queue_payload_track_preview(&evt.payload, "tracks"),
                                        "preview_autoplay_track_ids": queue_payload_track_preview(&evt.payload, "autoplay_tracks"),
                                    }),
                                })
                                .await;
                        }
                        TransportEvent::InboundRendererServerCommand(cmd) => {
                            log::debug!(
                                "[QConnect] <-- Inbound renderer command: {}",
                                cmd.message_type()
                            );
                        }
                        TransportEvent::InboundFrameDecoded {
                            cloud_message_type,
                            payload_size,
                        } => {
                            log::debug!(
                                "[QConnect/Transport] <-- Frame decoded: cloud_type={} size={}",
                                cloud_message_type,
                                payload_size
                            );
                        }
                        TransportEvent::InboundPayloadBytes {
                            cloud_message_type,
                            payload,
                        } => {
                            log::debug!(
                                "[QConnect/Transport] <-- Payload bytes: cloud_type={} len={}",
                                cloud_message_type,
                                payload.len()
                            );
                        }
                        TransportEvent::OutboundSent {
                            message_type,
                            action_uuid,
                        } => {
                            log::debug!(
                                "[QConnect/Transport] --> Outbound sent: {} uuid={}",
                                message_type,
                                action_uuid
                            );
                        }
                        TransportEvent::TransportError { stage, message } => {
                            log::error!(
                                "[QConnect/Transport] Error: stage={} message={}",
                                stage,
                                message
                            );
                        }
                        TransportEvent::CloudError { msg_id, code } => {
                            log::warn!(
                                "[QConnect/Transport] Cloud rejected session: msg_id={} code={} (issue #358)",
                                msg_id, code
                            );
                            self.sink
                                .on_event(QconnectAppEvent::Diagnostic {
                                    channel: "qconnect:cloud_error".to_string(),
                                    level: "warning".to_string(),
                                    payload: serde_json::json!({
                                        "msg_id": msg_id,
                                        "code": code,
                                        "category": "cloud-rejected",
                                    }),
                                })
                                .await;
                        }
                        TransportEvent::InboundReceived(_envelope) => {
                            log::info!("[QConnect/Transport] <-- InboundReceived (JSON envelope)");
                        }
                        TransportEvent::SessionEstablished => {
                            log::info!(
                                "[QConnect/Transport] Session established — backoff counters reset"
                            );
                            host.update_lifecycle(QconnectLifecycleState::Connected)
                                .await;
                        }
                        TransportEvent::MaxReconnectAttemptsExceeded {
                            attempts,
                            last_reason,
                        } => {
                            log::error!(
                                "[QConnect/Transport] Max reconnect attempts exceeded: attempts={} last_reason={}",
                                attempts,
                                last_reason
                            );
                            self.sink
                                .on_event(QconnectAppEvent::Diagnostic {
                                    channel: "qconnect:max_reconnect_attempts_exceeded".to_string(),
                                    level: "error".to_string(),
                                    payload: serde_json::json!({
                                        "attempts": attempts,
                                        "last_reason": last_reason,
                                    }),
                                })
                                .await;
                            // R3: the adapter owns the runtime/teardown. It sets
                            // Exhausted + last_error, drops the runtime unless idle-
                            // retry is active, surfaces the terminal status, and tells
                            // us whether to break (terminate) or keep idling.
                            let should_break = host
                                .on_reconnect_exhausted(
                                    *attempts,
                                    last_reason.clone(),
                                    idle_retry_active,
                                )
                                .await;
                            if should_break {
                                break;
                            }
                            // gap #7 (idle_retry_active): keep the loop alive for the
                            // re-armed transport; skip the reducer for this terminal
                            // diagnostic and wait for the next event.
                            continue;
                        }
                    }
                    if let Err(err) = self.handle_transport_event(event).await {
                        let message = format!("qconnect app transport handling error: {err}");
                        log::error!("{message}");
                        host.on_loop_error(message).await;
                    }
                    if received_authoritative_queue_state {
                        authoritative_queue_state_received = true;
                    }
                    if let Some(input) = takeover_input {
                        let has_playback_conflict = !playback_conflict_resolved
                            && input.was_playing
                            && matches!(
                                input.server,
                                ServerActiveState::OtherPlaying | ServerActiveState::OtherPaused
                            );
                        if has_playback_conflict {
                            // SESSION_STATE itself proves the session is ready.
                            // Surface Connected now so a user may consider the
                            // modal without racing the adapter's bootstrap timeout;
                            // all destructive follow-up events remain queued.
                            host.update_lifecycle(QconnectLifecycleState::Connected)
                                .await;
                            let choice = host
                                .resolve_local_playback_conflict(
                                    input.active_renderer_id.unwrap_or(-1),
                                    matches!(input.server, ServerActiveState::OtherPlaying),
                                )
                                .await;
                            playback_conflict_resolved = true;
                            match choice {
                                LocalPlaybackConflictChoice::ContinueOnActiveRenderer => {
                                    {
                                        let mut sync = self.sync.lock().await;
                                        crate::clear_local_queue_takeover(&mut sync);
                                        crate::set_local_playback_conflict_pending(
                                            &mut sync, false,
                                        );
                                    }
                                    // The initial QueueState may already have
                                    // been consumed while local playback was
                                    // fenced. Pull it again now that the user
                                    // explicitly chose remote authority.
                                    if let Err(error) = self.ask_for_queue_state().await {
                                        log::warn!(
                                            "[QConnect] remote queue refresh after peer choice failed: {error}"
                                        );
                                    }
                                    host.continue_active_renderer_playback().await;
                                    host.stop_local_playback_for_remote_queue().await;
                                }
                                LocalPlaybackConflictChoice::ContinueOnThisDevice => {
                                    let local_id = {
                                        let sync = self.sync.lock().await;
                                        sync.session.local_renderer_id
                                    };
                                    if authoritative_queue_state_received && local_id.is_some() {
                                        let local_id = local_id.expect("checked local renderer id");
                                        if let Err(error) =
                                            self.send_set_active_renderer(local_id).await
                                        {
                                            log::warn!(
                                                "[QConnect] remote-queue local takeover failed: {error}"
                                            );
                                            self.clear_local_playback_authority_fences().await;
                                            host.cancel_connection_for_playback_conflict().await;
                                            break;
                                        }
                                        host.stop_local_playback_for_remote_queue().await;
                                        {
                                            let mut sync = self.sync.lock().await;
                                            crate::clear_local_queue_takeover(&mut sync);
                                            crate::set_local_playback_conflict_pending(
                                                &mut sync, false,
                                            );
                                        }
                                        if let Err(error) = self.ask_for_queue_state().await {
                                            log::warn!(
                                                "[QConnect] remote queue refresh after local claim failed: {error}"
                                            );
                                        }
                                    } else {
                                        pending_remote_queue_takeover = true;
                                    }
                                }
                                LocalPlaybackConflictChoice::ContinueLocalPlaybackAndReplaceQueue => {
                                    // SET_ACTIVE_RENDERER is the handoff: the
                                    // previously-active peer is paused by the
                                    // session when QBZ becomes active. Sending a
                                    // separate STOP first occupies the pending
                                    // action slot and races this takeover.
                                    pending_local_takeover = true;
                                    pending_local_takeover_is_explicit = true;
                                }
                                LocalPlaybackConflictChoice::CancelConnection => {
                                    self.clear_local_playback_authority_fences().await;
                                    host.cancel_connection_for_playback_conflict().await;
                                    break;
                                }
                            }
                            // The explicit choice supersedes the generic matrix.
                            takeover_input = None;
                        }
                    }
                    // P1-3: now that the sink has applied the SESSION_STATE, run
                    // takeover arbitration on the prior snapshot captured above.
                    // Claim-active and local-queue publication are wired here;
                    // neither resets reconnect counters (#358 latch untouched).
                    if let Some(input) = takeover_input {
                        // Auto-take the render only on a FRESH connect (never a
                        // reconnect — that rejoins active via the RECONNECTION join
                        // reason), only when the cloud reports nobody active, and
                        // only ONCE per connect (the latch). This can never steal
                        // from an active peer: an active peer classifies as
                        // server==Other*/Me, not None.
                        let auto_take_when_idle = !has_disconnected
                            && matches!(input.server, ServerActiveState::None)
                            && !auto_take_attempted
                            && !pending_auto_take;
                        let decision = compute_connection_state(
                            input.was_active,
                            input.was_playing,
                            input.server,
                            // queue_equal is not yet derivable here without a
                            // controller/renderer queue diff; default false so the
                            // reduced matrix never spuriously suppresses a push.
                            false,
                            auto_take_when_idle,
                        );
                        if decision.should_set_queue {
                            // QueueLoadTracks is versioned. Do not publish from
                            // SESSION_STATE before the initial QueueState lands:
                            // that races with a stale version and the server
                            // rejects the action. Keep all remote queue/renderer
                            // effects fenced while the baseline is pending.
                            {
                                let mut sync = self.sync.lock().await;
                                crate::set_local_playback_conflict_pending(&mut sync, true);
                            }
                            pending_local_takeover = true;
                            pending_local_takeover_is_explicit = false;
                        }
                        if decision.should_set_active_renderer && !decision.should_set_queue {
                            let local_id = {
                                let st = self.sync.lock().await;
                                st.session.local_renderer_id
                            };
                            match local_id {
                                Some(local_id) => {
                                    match self.send_set_active_renderer(local_id).await {
                                        Ok(_) => {
                                            // A no-op means the cloud already
                                            // agrees that we are active.
                                            auto_take_attempted = true;
                                        }
                                        Err(err) => {
                                            log::warn!(
                                                    "[QConnect] takeover set_active_renderer failed: {err}"
                                                );
                                        }
                                    }
                                }
                                None if auto_take_when_idle => {
                                    // Our just-sent join is not yet confirmed, so
                                    // local_renderer_id is unknown. Defer to the
                                    // per-iteration check below, which fires once
                                    // the cloud's ADD_RENDERER for us lands.
                                    pending_auto_take = true;
                                }
                                None => {}
                            }
                        }
                    }

                    if pending_local_takeover {
                        let (local_id, active_peer_playing) = {
                            let st = self.sync.lock().await;
                            (
                                st.session.local_renderer_id,
                                crate::active_peer_renderer_is_playing(&st),
                            )
                        };
                        if !host.local_playback_is_playing()
                            || (active_peer_playing && !pending_local_takeover_is_explicit)
                        {
                            pending_local_takeover = false;
                            pending_local_takeover_is_explicit = false;
                            self.clear_local_playback_authority_fences().await;
                        } else if authoritative_queue_state_received && local_id.is_some() {
                            let local_id = local_id.expect("checked local renderer id");
                            let explicit = pending_local_takeover_is_explicit;
                            log::info!(
                                "[QConnect] local playback is authoritative; claiming renderer and publishing queue (local_id={local_id})"
                            );
                            if let Err(err) = self.send_set_active_renderer(local_id).await {
                                log::warn!(
                                    "[QConnect] local-playing takeover is waiting for the pending action to clear: {err}"
                                );
                            } else if !host.publish_local_queue_for_takeover().await {
                                log::warn!(
                                    "[QConnect] deferred local-playing takeover had no resolvable queue to publish"
                                );
                                pending_local_takeover = false;
                                pending_local_takeover_is_explicit = false;
                                if explicit {
                                    self.clear_local_playback_authority_fences().await;
                                }
                            } else {
                                pending_local_takeover = false;
                                pending_local_takeover_is_explicit = false;
                                auto_take_attempted = true;
                            }
                        }
                    }

                    if pending_remote_queue_takeover {
                        let local_id = {
                            let state = self.sync.lock().await;
                            state.session.local_renderer_id
                        };
                        if authoritative_queue_state_received && local_id.is_some() {
                            let local_id = local_id.expect("checked local renderer id");
                            pending_remote_queue_takeover = false;
                            if let Err(error) = self.send_set_active_renderer(local_id).await {
                                log::warn!(
                                    "[QConnect] deferred remote-queue local takeover failed: {error}"
                                );
                                self.clear_local_playback_authority_fences().await;
                                host.cancel_connection_for_playback_conflict().await;
                                break;
                            }
                            host.stop_local_playback_for_remote_queue().await;
                            {
                                let mut sync = self.sync.lock().await;
                                crate::clear_local_queue_takeover(&mut sync);
                                crate::set_local_playback_conflict_pending(&mut sync, false);
                            }
                            if let Err(error) = self.ask_for_queue_state().await {
                                log::warn!(
                                    "[QConnect] deferred remote queue refresh failed: {error}"
                                );
                            }
                        }
                    }

                    // Fire a deferred idle auto-take as soon as our
                    // local_renderer_id lands — but ONLY while the session is still
                    // idle (no active renderer), so we never steal a peer that
                    // became active while we waited for our id.
                    if pending_auto_take {
                        let (local_id, active_id) = {
                            let st = self.sync.lock().await;
                            (st.session.local_renderer_id, st.session.active_renderer_id)
                        };
                        if active_id.is_some() {
                            // Someone is active now — abandon the auto-take.
                            pending_auto_take = false;
                            auto_take_attempted = true;
                        } else if let Some(local_id) = local_id {
                            pending_auto_take = false;
                            auto_take_attempted = true;
                            log::info!(
                                "[QConnect] idle auto-take: claiming render (local_id={local_id})"
                            );
                            if let Err(err) = self.send_set_active_renderer(local_id).await {
                                log::warn!(
                                    "[QConnect] idle auto-take set_active_renderer failed: {err}"
                                );
                            }
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!("[QConnect] Transport event lagged by {skipped} messages");
                    // P1-8: a Lagged drop may have eaten the SESSION_STATE carrying
                    // the session_uuid. Re-ask for queue state until the uuid is
                    // confirmed or the attempt budget is spent.
                    let session_uuid_known = {
                        let st = self.sync.lock().await;
                        st.session.session_uuid.is_some()
                    };
                    if should_reask_queue_state(
                        session_uuid_known,
                        lagged_reask_attempts,
                        QCONNECT_REASK_QUEUE_STATE_MAX_ATTEMPTS,
                    ) {
                        lagged_reask_attempts = lagged_reask_attempts.saturating_add(1);
                        if let Err(err) = self.ask_for_queue_state().await {
                            log::warn!("[QConnect] Lagged-recovery AskForQueueState failed: {err}");
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    log::warn!("[QConnect/EventLoop] Transport channel closed, stopping");
                    break;
                }
            }
        }
        log::info!("[QConnect/EventLoop] Stopped");
    }
}

#[cfg(test)]
mod tests {
    use qbz_player::player::PlaybackBufferState;
    mod controller_takeover;

    use std::collections::VecDeque;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;

    use async_trait::async_trait;
    use qconnect_core::{QueueEvent, QueueVersion};
    use qconnect_transport_ws::{InMemoryWsTransport, TransportEvent, WsTransport};

    use crate::renderer::PLAYING_STATE_PLAYING;
    use crate::session::{
        LocalIdentity, QconnectLifecycleState, QconnectRendererInfo, ServerActiveState,
    };
    use crate::{QconnectAppError, QconnectRemoteSyncState};
    use serde_json::json;
    use tokio::sync::Mutex;

    use crate::{QconnectAppEvent, QconnectEventSink, JOIN_SESSION_REASON_CONTROLLER_REQUEST};

    use super::{map_server_event, queue_hashes_diverge, QconnectApp, SessionLoopHost};
    use qconnect_core::QConnectQueueState;
    use qconnect_protocol::{
        QueueCommand, QueueCommandType, QueueEventType, QueueServerEvent, RendererCommandType,
        RendererServerCommand,
    };
    use qconnect_transport_ws::WsTransportConfig;

    #[derive(Debug, Default, Clone)]
    struct TestSink {
        events: Arc<Mutex<Vec<QconnectAppEvent>>>,
        observed: Arc<std::sync::Mutex<Option<qbz_player::player::PlaybackEvent>>>,
        fail_execution: Arc<std::sync::atomic::AtomicBool>,
        volume_locked: Arc<std::sync::atomic::AtomicBool>,
    }

    impl TestSink {
        async fn snapshot(&self) -> Vec<QconnectAppEvent> {
            self.events.lock().await.clone()
        }
    }

    #[async_trait]
    impl QconnectEventSink for TestSink {
        fn allows_remote_volume(&self) -> bool {
            !self.volume_locked.load(Ordering::SeqCst)
        }

        fn playback_event(&self) -> Option<qbz_player::player::PlaybackEvent> {
            self.observed.lock().unwrap().clone()
        }
        async fn execute_renderer_command(&self, command: &qconnect_core::RendererCommand,
            state: &qconnect_core::QConnectRendererState) -> Result<(), String> {
            if self.fail_execution.load(Ordering::SeqCst) { return Err("injected engine rejection".into()); }
            self.on_event(QconnectAppEvent::RendererCommandApplied { command: command.clone(), state: state.clone() }).await;
            Ok(())
        }
        async fn on_event(&self, event: QconnectAppEvent) {
            self.events.lock().await.push(event);
        }
    }

    #[derive(Default)]
    struct TestSessionLoopHost {
        lifecycles: Mutex<Vec<QconnectLifecycleState>>,
        local_playing: bool,
        published_local_queues: AtomicUsize,
        conflict_choice: Option<super::LocalPlaybackConflictChoice>,
        cancelled_connections: AtomicUsize,
    }

    #[async_trait]
    impl SessionLoopHost for TestSessionLoopHost {
        fn local_playback_is_playing(&self) -> bool {
            self.local_playing
        }

        async fn publish_local_queue_for_takeover(&self) -> bool {
            self.published_local_queues.fetch_add(1, Ordering::SeqCst);
            true
        }

        async fn resolve_local_playback_conflict(
            &self,
            _active_renderer_id: i32,
            peer_was_playing: bool,
        ) -> super::LocalPlaybackConflictChoice {
            self.conflict_choice.unwrap_or(if peer_was_playing {
                super::LocalPlaybackConflictChoice::ContinueOnActiveRenderer
            } else {
                super::LocalPlaybackConflictChoice::ContinueLocalPlaybackAndReplaceQueue
            })
        }

        async fn cancel_connection_for_playback_conflict(&self) {
            self.cancelled_connections.fetch_add(1, Ordering::SeqCst);
        }

        async fn update_lifecycle(&self, state: QconnectLifecycleState) {
            self.lifecycles.lock().await.push(state);
        }

        async fn bootstrap_after_reconnect(&self) {}

        async fn deferred_renderer_join(&self, _session_uuid: String, _reason: i32) {}

        async fn on_reconnect_exhausted(
            &self,
            _attempts: u32,
            _last_reason: String,
            _idle_retry_active: bool,
        ) -> bool {
            true
        }

        async fn on_loop_error(&self, _message: String) {}
    }

    fn test_config() -> WsTransportConfig {
        let mut config = WsTransportConfig::default();
        config.endpoint_url = "wss://example.invalid/ws".to_string();
        config.subscribe_channels = vec![vec![1, 2, 3]];
        config
    }

    async fn build_connected_app() -> (
        QconnectApp<InMemoryWsTransport, TestSink>,
        TestSink,
        Arc<InMemoryWsTransport>,
        tokio::sync::broadcast::Receiver<qconnect_transport_ws::TransportEvent>,
    ) {
        let transport = Arc::new(InMemoryWsTransport::new());
        let sink = TestSink::default();
        let app = QconnectApp::new(
            Arc::clone(&transport),
            Arc::new(sink.clone()),
            Arc::new(Mutex::new(QconnectRemoteSyncState::default())),
        );
        let events_rx = app.subscribe_transport_events();
        app.connect(test_config()).await.expect("connect");
        (app, sink, transport, events_rx)
    }

    #[tokio::test]
    async fn delegated_join_report_owns_shared_initial_state_and_queue_version() {
        let (app, _sink, transport, _events_rx) = build_connected_app().await;
        {
            let state = app.state_handle();
            state.lock().await.queue.version = QueueVersion::new(7, 3);
        }

        let session_id = "98f0c227-767c-4a4f-bb1c-69fccb4cf6d5";
        app.send_delegated_join(
            session_id,
            true,
            json!({
                "device_uuid": "13d40c34-df5d-4eb5-a513-e5145364a800",
                "capabilities": {"max_audio_quality": 4}
            }),
        )
        .await
        .expect("delegated join");

        let sent = transport.sent_messages().await;
        assert_eq!(sent.len(), 1);
        let envelope = &sent[0];
        assert_eq!(envelope.message_type, "MESSAGE_TYPE_RNDR_SRVR_JOIN_SESSION");
        assert_eq!(envelope.queue_version_ref, QueueVersion::new(7, 3));
        assert_eq!(envelope.payload["session_uuid"], session_id);
        assert_eq!(envelope.payload["is_active"], true);
        assert_eq!(
            envelope.payload["reason"],
            JOIN_SESSION_REASON_CONTROLLER_REQUEST
        );
        assert_eq!(envelope.payload["initial_state"]["playing_state"], 1);
        assert_eq!(envelope.payload["initial_state"]["buffer_state"], 2);
        assert_eq!(
            envelope.payload["initial_state"]["queue_version"],
            json!({"major": 7, "minor": 3})
        );
        assert!(envelope.payload_bytes.is_some());
    }

    #[tokio::test]
    async fn session_loop_drains_prefetched_events_once_before_live_receiver() {
        // Build without `app.connect()`: that helper intentionally emits its own
        // TransportConnected sink event, which is outside the prefetched/live
        // receiver sequence this test measures.
        let transport = Arc::new(InMemoryWsTransport::new());
        let sink = TestSink::default();
        let app = QconnectApp::new(
            transport,
            Arc::new(sink.clone()),
            Arc::new(Mutex::new(QconnectRemoteSyncState::default())),
        );
        let (events_tx, mut events_rx) = tokio::sync::broadcast::channel(8);
        events_tx
            .send(qconnect_transport_ws::TransportEvent::Connected)
            .expect("preflight receiver exists");
        events_tx
            .send(qconnect_transport_ws::TransportEvent::SessionEstablished)
            .expect("preflight receiver exists");
        let prefetched = VecDeque::from([
            events_rx.recv().await.expect("prefetched connected event"),
            events_rx
                .recv()
                .await
                .expect("prefetched established event"),
        ]);
        events_tx
            .send(qconnect_transport_ws::TransportEvent::Disconnected)
            .expect("live receiver exists");
        drop(events_tx);

        let host = Arc::new(TestSessionLoopHost::default());
        let loop_host: Arc<dyn SessionLoopHost> = host.clone();
        app.run_session_loop_with_prefetched(loop_host, events_rx, false, prefetched)
            .await;

        assert_eq!(
            *host.lifecycles.lock().await,
            vec![
                QconnectLifecycleState::Connected,
                QconnectLifecycleState::Reconnecting,
            ]
        );
        let sink_events = sink.snapshot().await;
        assert_eq!(
            sink_events
                .iter()
                .filter(|event| matches!(event, QconnectAppEvent::TransportConnected))
                .count(),
            1
        );
        assert_eq!(
            sink_events
                .iter()
                .filter(|event| matches!(event, QconnectAppEvent::TransportDisconnected))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn local_playing_takeover_waits_for_authoritative_queue_version() {
        let transport = Arc::new(InMemoryWsTransport::new());
        let sink = TestSink::default();
        let app = QconnectApp::new(
            transport,
            Arc::new(sink),
            Arc::new(Mutex::new(QconnectRemoteSyncState::default())),
        );
        {
            let sync = app.sync_handle();
            let mut state = sync.lock().await;
            state.session.local_renderer_id = Some(1);
            state.session.active_renderer_id = Some(1);
        }

        let session_state = TransportEvent::InboundQueueServerEvent(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlSessionState,
            action_uuid: None,
            queue_version: Some(QueueVersion::new(4, 1)),
            payload: json!({
                "session_uuid": "session-1",
                "active_renderer_id": 1,
                "playing_state": PLAYING_STATE_PLAYING,
            }),
        });

        // SESSION_STATE alone must not publish with the default/stale version.
        let (events_tx, events_rx) = tokio::sync::broadcast::channel(4);
        drop(events_tx);
        let host = Arc::new(TestSessionLoopHost {
            local_playing: true,
            ..Default::default()
        });
        app.run_session_loop_with_prefetched(
            host.clone(),
            events_rx,
            false,
            VecDeque::from([session_state.clone()]),
        )
        .await;
        assert_eq!(host.published_local_queues.load(Ordering::SeqCst), 0);

        // Once QueueState has updated qconnect-app's version, the deferred
        // takeover may publish exactly once.
        let (events_tx, events_rx) = tokio::sync::broadcast::channel(4);
        drop(events_tx);
        let host = Arc::new(TestSessionLoopHost {
            local_playing: true,
            ..Default::default()
        });
        let queue_state = TransportEvent::InboundQueueServerEvent(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlQueueState,
            action_uuid: None,
            queue_version: Some(QueueVersion::new(4, 1)),
            payload: json!({ "tracks": [], "shuffle_mode": false }),
        });
        app.run_session_loop_with_prefetched(
            host.clone(),
            events_rx,
            false,
            VecDeque::from([session_state, queue_state]),
        )
        .await;
        assert_eq!(host.published_local_queues.load(Ordering::SeqCst), 1);
        assert_eq!(
            app.queue_state_snapshot().await.version,
            QueueVersion::new(4, 1)
        );
    }

    #[tokio::test]
    async fn explicit_local_queue_takeover_retries_without_disconnecting() {
        let transport = Arc::new(InMemoryWsTransport::new());
        let sink = TestSink::default();
        let app = QconnectApp::new(
            Arc::clone(&transport),
            Arc::new(sink),
            Arc::new(Mutex::new(QconnectRemoteSyncState::default())),
        );
        let _transport_events = app.subscribe_transport_events();
        app.connect(test_config()).await.expect("connect transport");
        {
            let sync = app.sync_handle();
            let mut state = sync.lock().await;
            state.session.local_renderer_id = Some(3);
            state.session.active_renderer_id = Some(7);
        }

        // Reproduce the field failure: another queue action occupies the slot
        // when option 3 first tries to claim this renderer.
        let existing = app
            .build_queue_command(
                QueueCommandType::CtrlSrvrQueueAddTracks,
                json!({ "track_ids": [99] }),
            )
            .await;
        let existing_uuid = app
            .send_queue_command(existing)
            .await
            .expect("seed pending queue action");

        let session_state = TransportEvent::InboundQueueServerEvent(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlSessionState,
            action_uuid: None,
            queue_version: Some(QueueVersion::new(11, 1)),
            payload: json!({
                "session_uuid": "session-option-3",
                "active_renderer_id": 7,
                "playing_state": PLAYING_STATE_PLAYING,
            }),
        });
        let queue_state = TransportEvent::InboundQueueServerEvent(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlQueueState,
            action_uuid: None,
            queue_version: Some(QueueVersion::new(11, 1)),
            payload: json!({ "tracks": [], "shuffle_mode": false }),
        });
        let pending_completed = TransportEvent::InboundQueueServerEvent(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlQueueTracksAdded,
            action_uuid: Some(existing_uuid),
            queue_version: Some(QueueVersion::new(11, 2)),
            payload: json!({ "tracks": [] }),
        });

        let (events_tx, events_rx) = tokio::sync::broadcast::channel(4);
        drop(events_tx);
        let host = Arc::new(TestSessionLoopHost {
            local_playing: true,
            conflict_choice: Some(
                super::LocalPlaybackConflictChoice::ContinueLocalPlaybackAndReplaceQueue,
            ),
            ..Default::default()
        });
        app.run_session_loop_with_prefetched(
            host.clone(),
            events_rx,
            false,
            VecDeque::from([session_state, queue_state, pending_completed]),
        )
        .await;

        assert_eq!(host.published_local_queues.load(Ordering::SeqCst), 1);
        assert_eq!(host.cancelled_connections.load(Ordering::SeqCst), 0);
        let sent = transport.sent_messages().await;
        assert_eq!(
            sent.iter()
                .filter(|message| {
                    message.message_type == "MESSAGE_TYPE_CTRL_SRVR_SET_ACTIVE_RENDERER"
                })
                .count(),
            1
        );
        let set_active = sent
            .iter()
            .find(|message| message.message_type == "MESSAGE_TYPE_CTRL_SRVR_SET_ACTIVE_RENDERER")
            .expect("option 3 must claim the local renderer");
        assert_eq!(set_active.payload["renderer_id"], 3);
        assert!(sent
            .iter()
            .all(|message| { message.message_type != "MESSAGE_TYPE_CTRL_SRVR_SET_PLAYER_STATE" }));
    }

    fn renderer_info(renderer_id: i32, device_uuid: &str) -> QconnectRendererInfo {
        QconnectRendererInfo {
            renderer_id,
            device_uuid: Some(device_uuid.to_string()),
            friendly_name: None,
            brand: None,
            model: None,
            device_type: None,
            volume_remote_control: None,
        }
    }

    fn local_identity(device_uuid: &str) -> LocalIdentity {
        LocalIdentity {
            device_uuid: device_uuid.to_string(),
            ..Default::default()
        }
    }

    /// Seed a local(1) + peer(2) topology with the peer as the active renderer.
    async fn seed_local_and_active_peer(app: &QconnectApp<InMemoryWsTransport, TestSink>) {
        let handle = app.sync_handle();
        let mut state = handle.lock().await;
        state.session.local_renderer_id = Some(1);
        state.session.active_renderer_id = Some(2);
        state.session.renderers = vec![
            renderer_info(1, "local-uuid"),
            renderer_info(2, "peer-uuid"),
        ];
    }

    /// SESSION_STATE writes session topology under the sync lock and reports the
    /// post-lock work (loop mode + local-playback handoff) in the outcome.
    #[tokio::test]
    async fn apply_session_state_sets_topology_and_returns_outcome() {
        let (app, _sink, _transport, _rx) = build_connected_app().await;
        let outcome = app
            .apply_session_management_event(
                "MESSAGE_TYPE_SRVR_CTRL_SESSION_STATE",
                &json!({
                    "session_uuid": "sess-1",
                    "active_renderer_id": 2,
                    "playing_state": PLAYING_STATE_PLAYING,
                    "loop_mode": 3
                }),
                &local_identity("local-uuid"),
            )
            .await;
        assert!(outcome.sync_local_playback);
        assert_eq!(outcome.apply_loop_mode, Some(3));
        let handle = app.sync_handle();
        let state = handle.lock().await;
        assert_eq!(state.session.session_uuid.as_deref(), Some("sess-1"));
        assert_eq!(state.session.active_renderer_id, Some(2));
        assert_eq!(state.session_loop_mode, Some(3));
        assert_eq!(
            state
                .session_renderer_states
                .get(&2)
                .and_then(|renderer| renderer.playing_state),
            Some(PLAYING_STATE_PLAYING)
        );
    }

    #[tokio::test]
    async fn local_to_peer_handoff_clears_unfinished_local_takeover() {
        let (app, _sink, _transport, _rx) = build_connected_app().await;
        {
            let handle = app.sync_handle();
            let mut state = handle.lock().await;
            state.session.local_renderer_id = Some(1);
            state.session.active_renderer_id = Some(1);
            crate::arm_local_queue_takeover(
                &mut state,
                vec![10, 20],
                "load-before-handoff".to_string(),
            );
        }

        app.apply_session_management_event(
            "MESSAGE_TYPE_SRVR_CTRL_ACTIVE_RENDERER_CHANGED",
            &json!({ "active_renderer_id": 2 }),
            &local_identity("local-uuid"),
        )
        .await;

        let handle = app.sync_handle();
        let state = handle.lock().await;
        assert_eq!(state.session.active_renderer_id, Some(2));
        assert!(state.pending_local_queue_takeover.is_none());
    }

    /// Repeated server snapshots are common during bootstrap. Applying the
    /// same loop mode twice would echo another renderer command and was one of
    /// the ingredients in the SET_LOOP_MODE feedback storm seen in the field.
    #[tokio::test]
    async fn repeated_loop_mode_is_not_reapplied() {
        let (app, _sink, _transport, _rx) = build_connected_app().await;
        let identity = local_identity("local-uuid");

        let first = app
            .apply_session_management_event(
                "MESSAGE_TYPE_SRVR_CTRL_LOOP_MODE_SET",
                &json!({ "loop_mode": 3 }),
                &identity,
            )
            .await;
        let repeated = app
            .apply_session_management_event(
                "MESSAGE_TYPE_SRVR_CTRL_LOOP_MODE_SET",
                &json!({ "loop_mode": 3 }),
                &identity,
            )
            .await;

        assert_eq!(first.apply_loop_mode, Some(3));
        assert_eq!(repeated.apply_loop_mode, None);
    }

    /// Regression (take-renderer-when-idle): the cloud encodes "no active
    /// renderer" as `active_renderer_id: -1`. The takeover classifier must
    /// normalize it to `ServerActiveState::None` — identical to the apply
    /// path's `normalize_active_renderer_id` — so the idle auto-take fires on a
    /// fresh connect. A raw parse mis-read -1 as `Some(-1)` (a peer is active),
    /// classified it as `OtherPaused`, and silently suppressed the auto-take.
    #[tokio::test]
    async fn capture_takeover_treats_negative_active_renderer_as_none() {
        let (app, _sink, _transport, _rx) = build_connected_app().await;
        {
            let handle = app.sync_handle();
            let mut state = handle.lock().await;
            state.session.local_renderer_id = Some(8);
        }
        let input = app
            .capture_session_state_takeover_input(&json!({ "active_renderer_id": -1 }), false)
            .await;
        assert_eq!(input.server, ServerActiveState::None);
    }

    /// A real active peer (id >= 0, not us) still classifies as a peer, so the
    /// auto-take never steals it.
    #[tokio::test]
    async fn capture_takeover_treats_active_peer_as_other() {
        let (app, _sink, _transport, _rx) = build_connected_app().await;
        {
            let handle = app.sync_handle();
            let mut state = handle.lock().await;
            state.session.local_renderer_id = Some(8);
        }
        let input = app
            .capture_session_state_takeover_input(&json!({ "active_renderer_id": 5 }), false)
            .await;
        assert_eq!(input.active_renderer_id, Some(5));
        assert!(matches!(
            input.server,
            ServerActiveState::OtherPlaying | ServerActiveState::OtherPaused
        ));
    }

    #[tokio::test]
    async fn conflict_fences_are_cleared_before_a_later_connection() {
        let (app, _sink, _transport, _rx) = build_connected_app().await;
        {
            let handle = app.sync_handle();
            let mut state = handle.lock().await;
            state.local_playback_conflict_pending = true;
            crate::arm_local_queue_takeover(&mut state, vec![101, 102], "load-1".to_string());
        }

        app.clear_local_playback_authority_fences().await;

        let handle = app.sync_handle();
        let state = handle.lock().await;
        assert!(!state.local_playback_conflict_pending);
        assert!(state.pending_local_queue_takeover.is_none());
    }

    #[tokio::test]
    async fn capture_takeover_uses_real_local_playback_and_incoming_peer_state() {
        let (app, _sink, _transport, _rx) = build_connected_app().await;
        {
            let handle = app.sync_handle();
            let mut state = handle.lock().await;
            state.session.local_renderer_id = Some(8);
            // This unrelated legacy snapshot must not classify renderer 5.
            state.last_renderer_playing_state = Some(PLAYING_STATE_PLAYING);
        }

        let paused = app
            .capture_session_state_takeover_input(
                &json!({ "active_renderer_id": 5, "playing_state": 3 }),
                true,
            )
            .await;
        assert!(paused.was_playing);
        assert_eq!(paused.server, ServerActiveState::OtherPaused);

        let playing = app
            .capture_session_state_takeover_input(
                &json!({ "active_renderer_id": 5, "playing_state": 2 }),
                true,
            )
            .await;
        assert_eq!(playing.server, ServerActiveState::OtherPlaying);
    }

    /// A RENDERER_STATE_UPDATED for a PLAYING active peer arms the watchdog and
    /// reports the renderer for projection.
    #[tokio::test]
    async fn renderer_state_updated_arms_watchdog_for_playing_active_peer() {
        let (app, _sink, _transport, _rx) = build_connected_app().await;
        seed_local_and_active_peer(&app).await;
        let outcome = app
            .apply_session_management_event(
                "MESSAGE_TYPE_SRVR_CTRL_RENDERER_STATE_UPDATED",
                &json!({ "renderer_id": 2, "player_state": { "playing_state": 2 } }),
                &local_identity("local-uuid"),
            )
            .await;
        let (renderer_id, _generation) = outcome
            .watchdog_arm
            .expect("watchdog should arm for a playing active peer");
        assert_eq!(renderer_id, 2);
        assert_eq!(outcome.remote_projection_renderer_id, Some(2));
    }

    /// The full arm -> 12s silence -> fire path: emits RendererUnreachable and
    /// freezes the cached projection to UNKNOWN(0).
    #[tokio::test(start_paused = true)]
    async fn watchdog_fires_after_silence_and_freezes_projection() {
        let (app, sink, _transport, _rx) = build_connected_app().await;
        seed_local_and_active_peer(&app).await;
        let outcome = app
            .apply_session_management_event(
                "MESSAGE_TYPE_SRVR_CTRL_RENDERER_STATE_UPDATED",
                &json!({ "renderer_id": 2, "player_state": { "playing_state": 2 } }),
                &local_identity("local-uuid"),
            )
            .await;
        let (renderer_id, generation) = outcome.watchdog_arm.expect("arm");
        app.arm_renderer_watchdog(renderer_id, generation);

        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(12_000 + 10)).await;
        tokio::task::yield_now().await;

        let events = sink.snapshot().await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, QconnectAppEvent::RendererUnreachable { renderer_id: 2 })),
            "expected RendererUnreachable for renderer 2"
        );
        let handle = app.sync_handle();
        let state = handle.lock().await;
        assert_eq!(
            state
                .session_renderer_states
                .get(&2)
                .and_then(|r| r.playing_state),
            Some(0),
            "frozen renderer playing_state must be UNKNOWN(0)"
        );
    }

    /// Atomicity regression: a stale-epoch watchdog must no-op. A subsequent
    /// non-playing update bumps the epoch (disarm) without arming a new task, so
    /// the first task wakes to a superseded generation and does NOT fire.
    #[tokio::test(start_paused = true)]
    async fn watchdog_disarmed_by_epoch_bump_does_not_fire() {
        let (app, sink, _transport, _rx) = build_connected_app().await;
        seed_local_and_active_peer(&app).await;
        let armed = app
            .apply_session_management_event(
                "MESSAGE_TYPE_SRVR_CTRL_RENDERER_STATE_UPDATED",
                &json!({ "renderer_id": 2, "player_state": { "playing_state": 2 } }),
                &local_identity("local-uuid"),
            )
            .await;
        let (renderer_id, generation) = armed.watchdog_arm.expect("arm");
        app.arm_renderer_watchdog(renderer_id, generation);

        // A paused update bumps the watchdog epoch and does NOT arm a new task.
        let disarm = app
            .apply_session_management_event(
                "MESSAGE_TYPE_SRVR_CTRL_RENDERER_STATE_UPDATED",
                &json!({ "renderer_id": 2, "player_state": { "playing_state": 3 } }),
                &local_identity("local-uuid"),
            )
            .await;
        assert!(disarm.watchdog_arm.is_none());

        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(12_000 + 10)).await;
        tokio::task::yield_now().await;

        let events = sink.snapshot().await;
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, QconnectAppEvent::RendererUnreachable { .. })),
            "stale-epoch watchdog must not fire after a disarming bump"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_aborts_runtime_owned_watchdog() {
        let (app, sink, _transport, _rx) = build_connected_app().await;
        seed_local_and_active_peer(&app).await;
        let armed = app
            .apply_session_management_event(
                "MESSAGE_TYPE_SRVR_CTRL_RENDERER_STATE_UPDATED",
                &json!({ "renderer_id": 2, "player_state": { "playing_state": 2 } }),
                &local_identity("local-uuid"),
            )
            .await;
        let (renderer_id, generation) = armed.watchdog_arm.expect("arm");
        app.arm_renderer_watchdog(renderer_id, generation);

        app.disconnect().await.expect("disconnect");
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(12_010)).await;
        tokio::task::yield_now().await;

        assert!(
            !sink
                .snapshot()
                .await
                .iter()
                .any(|event| matches!(event, QconnectAppEvent::RendererUnreachable { .. })),
            "a retired app watchdog must not retain or publish through its sink"
        );
    }

    /// ACTIVE_DISCONNECTED(status==2) for the active peer reports the
    /// disconnected renderer; freezing emits RendererDisconnected + UNKNOWN(0).
    #[tokio::test]
    async fn active_disconnected_reports_id_and_freeze_emits() {
        let (app, sink, _transport, _rx) = build_connected_app().await;
        seed_local_and_active_peer(&app).await;
        let outcome = app
            .apply_session_management_event(
                "MESSAGE_TYPE_SRVR_CTRL_RENDERER_STATE_UPDATED",
                &json!({ "renderer_id": 2, "status": 2, "player_state": { "playing_state": 2 } }),
                &local_identity("local-uuid"),
            )
            .await;
        assert_eq!(outcome.disconnected_renderer_id, Some(2));

        app.freeze_active_renderer_projection(
            2,
            QconnectAppEvent::RendererDisconnected { renderer_id: 2 },
        )
        .await;

        let events = sink.snapshot().await;
        assert!(events
            .iter()
            .any(|e| matches!(e, QconnectAppEvent::RendererDisconnected { renderer_id: 2 })));
        let handle = app.sync_handle();
        let state = handle.lock().await;
        assert_eq!(
            state
                .session_renderer_states
                .get(&2)
                .and_then(|r| r.playing_state),
            Some(0)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pending_timeout_triggers_resync() {
        let (app, sink, transport, _events_rx) = build_connected_app().await;
        let command = app
            .build_queue_command(
                QueueCommandType::CtrlSrvrQueueAddTracks,
                json!({"track_ids":[101]}),
            )
            .await;
        app.send_queue_command(command)
            .await
            .expect("send queue command");

        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(
            QconnectApp::<InMemoryWsTransport, TestSink>::PENDING_ACTION_TIMEOUT_MS + 10,
        ))
        .await;
        tokio::task::yield_now().await;

        let events = sink.snapshot().await;
        assert!(events
            .iter()
            .any(|event| { matches!(event, QconnectAppEvent::PendingActionTimedOut { .. }) }));
        assert!(events
            .iter()
            .any(|event| matches!(event, QconnectAppEvent::QueueResyncTriggered)));

        let sent = transport.sent_messages().await;
        assert!(
            sent.len() >= 2,
            "expected original command plus ask-for-state resync"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_aborts_pending_timeout_and_resync() {
        let (app, sink, transport, _events_rx) = build_connected_app().await;
        let command = app
            .build_queue_command(
                QueueCommandType::CtrlSrvrQueueAddTracks,
                json!({"track_ids":[101]}),
            )
            .await;
        app.send_queue_command(command)
            .await
            .expect("send queue command");
        let sent_before_disconnect = transport.sent_messages().await.len();

        app.disconnect().await.expect("disconnect");
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(
            QconnectApp::<InMemoryWsTransport, TestSink>::PENDING_ACTION_TIMEOUT_MS + 10,
        ))
        .await;
        tokio::task::yield_now().await;

        let events = sink.snapshot().await;
        assert!(!events
            .iter()
            .any(|event| matches!(event, QconnectAppEvent::PendingActionTimedOut { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event, QconnectAppEvent::QueueResyncTriggered)));
        assert_eq!(
            transport.sent_messages().await.len(),
            sent_before_disconnect,
            "retired timeout must not send ask-for-state"
        );
    }

    #[tokio::test]
    async fn concurrent_remote_event_cancels_pending_and_requests_resync() {
        let (app, sink, transport, _events_rx) = build_connected_app().await;
        let command = app
            .build_queue_command(
                QueueCommandType::CtrlSrvrQueueAddTracks,
                json!({"track_ids":[101]}),
            )
            .await;
        let pending_uuid = app
            .send_queue_command(command)
            .await
            .expect("send queue command");

        let remote_event = QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlQueueTracksAdded,
            action_uuid: Some("0f892e1a-a2f4-4d18-82c6-31e8daf2ea0f".to_string()),
            queue_version: Some(QueueVersion::new(1, 1)),
            payload: json!({"tracks":[]}),
        };

        app.apply_server_event(remote_event)
            .await
            .expect("apply concurrent event");

        let events = sink.snapshot().await;
        assert!(events.iter().any(|event| {
            matches!(
                event,
                QconnectAppEvent::PendingActionCanceledByConcurrentRemoteEvent { pending_uuid: id, .. } if id == &pending_uuid
            )
        }));
        assert!(events
            .iter()
            .any(|event| matches!(event, QconnectAppEvent::QueueResyncTriggered)));

        let sent = transport.sent_messages().await;
        assert!(
            sent.len() >= 2,
            "expected original command plus ask-for-state resync"
        );
    }

    #[test]
    fn authoritative_queue_state_without_indexes_drops_stale_shuffle_order() {
        let current = QConnectQueueState {
            shuffle_mode: true,
            shuffle_order: Some(vec![1, 0]),
            ..Default::default()
        };
        let event = QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlQueueState,
            action_uuid: None,
            queue_version: Some(QueueVersion::new(2, 0)),
            payload: json!({
                "tracks": [
                    {"track_id": 10, "queue_item_id": 100},
                    {"track_id": 20, "queue_item_id": 200}
                ],
                "shuffle_mode": true
            }),
        };

        let QueueEvent::QueueStateReplaced { state, .. } = map_server_event(&event, &current)
        else {
            panic!("expected queue-state replacement");
        };

        assert_eq!(state.queue_items.len(), 2);
        assert_eq!(state.shuffle_order, None);
    }

    mod controller_smoke {
        use super::*;

        #[tokio::test]
        async fn renderer_volume_capabilities_survive_wire_add_and_update() {
            use prost::Message;
            use qconnect_protocol::{decode_queue_server_events, QConnectMessage, QConnectMessages};
            let (app, _sink, transport, _events_rx) = build_connected_app().await;
            app.trigger_queue_state_resync().await;
            let query_uuid = app
                .state
                .lock()
                .await
                .pending
                .current()
                .unwrap()
                .uuid
                .clone();
            // Schema-shaped protobuf frames, not already-decoded JSON: the
            // decoder used to drop capabilities before the session saw them.
            for (update, capability, expected) in [
                (false, Some(1u8), 1),
                (true, Some(2), 2),
                (true, Some(1), 1),
                (true, None, 1),
            ] {
                let mut info = vec![0x12, 5, b'p', b'h', b'o', b'n', b'e'];
                if let Some(value) = capability {
                    info.extend_from_slice(&[0x3a, 2, 0x18, value]);
                }
                let mut payload = vec![0x08, 2, 0x12, info.len() as u8];
                payload.extend(info);
                let mut frame = vec![
                    0x08,
                    if update { 84 } else { 83 },
                    if update { 0xa2 } else { 0x9a },
                    0x05,
                    payload.len() as u8,
                ];
                frame.extend(payload);
                let batch = QConnectMessages {
                    messages: vec![QConnectMessage::decode(frame.as_slice()).unwrap()],
                    ..Default::default()
                };
                for event in decode_queue_server_events(&batch.encode_to_vec()).unwrap() {
                    app.apply_session_management_event(
                        event.event_type.as_message_type(),
                        &event.payload,
                        &local_identity("local-uuid"),
                    )
                    .await;
                }
                let state = app.sync.lock().await;
                let peer = state
                    .session
                    .renderers
                    .iter()
                    .find(|r| r.renderer_id == 2)
                    .unwrap();
                assert_eq!(peer.volume_remote_control, Some(expected));
                assert_eq!(crate::renderer_allows_remote_volume(peer), expected == 2);
                drop(state);
                // Exercise the transport boundary, not just its policy helper.
                // Repeated forbidden volume/mute requests emit no frames and
                // preserve an outstanding queue read. Permission updates take
                // effect immediately for both controls.
                for command_type in [
                    QueueCommandType::CtrlSrvrSetVolume,
                    QueueCommandType::CtrlSrvrMuteVolume,
                ] {
                    for volume in [20, 40, 60] {
                        let before = transport.sent_messages().await.len();
                        let command = app
                            .build_queue_command(
                                command_type,
                                json!({"renderer_id": 2, "volume": volume, "value": true}),
                            )
                            .await;
                        let result = app.send_queue_command(command).await;
                        if expected == 2 {
                            assert!(result.is_ok(), "{result:?}");
                            assert_eq!(transport.sent_messages().await.len(), before + 1);
                        } else {
                            assert!(matches!(
                                result,
                                Err(QconnectAppError::RemoteVolumeRestricted)
                            ));
                            assert_eq!(transport.sent_messages().await.len(), before);
                        }
                        assert_eq!(
                            app.state.lock().await.pending.current().unwrap().uuid,
                            query_uuid
                        );
                    }
                }
            }
            app.disconnect().await.unwrap();
        }

        #[tokio::test]
        async fn foreign_queue_delta_does_not_replace_the_pending_read() {
            let (app, sink, transport, _events_rx) = build_connected_app().await;
            app.trigger_queue_state_resync().await;
            let query_uuid = app
                .state_handle()
                .lock()
                .await
                .pending
                .current()
                .unwrap()
                .uuid
                .clone();
            app.apply_server_event(QueueServerEvent {
                event_type: QueueEventType::SrvrCtrlQueueTracksRemoved,
                action_uuid: Some("00000000-0000-4000-8000-000000000001".into()),
                queue_version: Some(QueueVersion::new(2, 1)),
                payload: json!({"queue_item_ids": [100]}),
            })
            .await
            .unwrap();
            assert_eq!(
                app.state_handle()
                    .lock()
                    .await
                    .pending
                    .current()
                    .unwrap()
                    .uuid,
                query_uuid
            );
            assert_eq!(
                app.queue_state_snapshot().await.version,
                QueueVersion::new(2, 1)
            );
            assert_eq!(transport.sent_messages().await.len(), 1);
            assert!(!sink.snapshot().await.iter().any(|event| matches!(
                event,
                QconnectAppEvent::PendingActionCanceledByConcurrentRemoteEvent { .. }
            )));
            app.disconnect().await.unwrap();
        }

        #[tokio::test]
        async fn bypassed_control_notifications_preserve_read_until_its_snapshot() {
            let (app, _sink, _transport, _events_rx) = build_connected_app().await;
            app.trigger_queue_state_resync().await;
            let query_uuid = app
                .state_handle()
                .lock()
                .await
                .pending
                .current()
                .unwrap()
                .uuid
                .clone();
            let pause = app
                .build_queue_command(
                    QueueCommandType::CtrlSrvrSetPlayerState,
                    json!({"playing_state": 3}),
                )
                .await;
            let pause_uuid = app.send_queue_command(pause).await.unwrap();
            for action_uuid in [None, Some(pause_uuid)] {
                for (event_type, payload) in [
                    (
                        QueueEventType::SrvrCtrlRendererStateUpdated,
                        json!({"renderer_id": 7, "playing_state": 3}),
                    ),
                    (
                        QueueEventType::SrvrCtrlVolumeChanged,
                        json!({"renderer_id": 7, "volume": 50}),
                    ),
                    (
                        QueueEventType::SrvrCtrlActiveRendererChanged,
                        json!({"renderer_id": 7}),
                    ),
                ] {
                    app.apply_server_event(QueueServerEvent {
                        event_type,
                        action_uuid: action_uuid.clone(),
                        queue_version: None,
                        payload,
                    })
                    .await
                    .unwrap();
                    assert_eq!(
                        app.state_handle()
                            .lock()
                            .await
                            .pending
                            .current()
                            .unwrap()
                            .uuid,
                        query_uuid
                    );
                }
            }
            app.apply_server_event(QueueServerEvent {
                event_type: QueueEventType::SrvrCtrlQueueState,
                action_uuid: Some(query_uuid),
                queue_version: Some(QueueVersion::new(2, 1)),
                payload: json!({"tracks": [], "shuffle_mode": false}),
            })
            .await
            .unwrap();
            assert!(app.state_handle().lock().await.pending.current().is_none());
            app.disconnect().await.unwrap();
        }

        #[tokio::test]
        async fn complete_shuffle_mode_set_does_not_request_another_snapshot() {
            let (app, sink, transport, _events_rx) = build_connected_app().await;

            {
                let state = app.state_handle();
                let mut state = state.lock().await;
                state.queue.version = QueueVersion::new(1, 1);
                state.queue.queue_items = [100, 200]
                    .into_iter()
                    .map(|id| qconnect_core::QueueItem {
                        track_id: id,
                        queue_item_id: id,
                        track_context_uuid: String::new(),
                    })
                    .collect();
            }

            app.apply_server_event(QueueServerEvent {
                event_type: QueueEventType::SrvrCtrlShuffleModeSet,
                action_uuid: Some("6f9f8d84-cd82-486f-a423-cd467117f39d".to_string()),
                queue_version: Some(QueueVersion::new(1, 2)),
                payload: json!({
                    "shuffle_mode": true,
                    "shuffle_seed": 123,
                    "shuffle_pivot_queue_item_id": 200,
                    "autoplay_reset": false,
                    "autoplay_loading": false
                }),
            })
            .await
            .expect("apply shuffle-mode-set event");

            assert_eq!(
                app.queue_state_snapshot().await.shuffle_order,
                Some(vec![1, 0])
            );
            let events = sink.snapshot().await;
            assert!(events
                .iter()
                .any(|event| matches!(event, QconnectAppEvent::QueueUpdated(_))));
            assert!(!events
                .iter()
                .any(|event| matches!(event, QconnectAppEvent::QueueResyncTriggered)));

            let sent = transport.sent_messages().await;
            assert!(
                sent.is_empty(),
                "an authoritative seed is sufficient; querying again can feed a snapshot loop"
            );
        }

        #[tokio::test]
        async fn repeated_snapshot_shuffle_and_loop_notifications_leave_controls_available() {
            let (app, _sink, transport, _events_rx) = build_connected_app().await;
            for _ in 0..5 {
                for (event_type, payload) in [
                    (
                        QueueEventType::SrvrCtrlQueueState,
                        json!({"tracks": [], "shuffle_mode": false}),
                    ),
                    (
                        QueueEventType::SrvrCtrlShuffleModeSet,
                        json!({"shuffle_mode": false}),
                    ),
                    (QueueEventType::SrvrCtrlLoopModeSet, json!({"loop_mode": 1})),
                ] {
                    app.apply_server_event(QueueServerEvent {
                        event_type,
                        action_uuid: None,
                        queue_version: Some(QueueVersion::new(2, 1)),
                        payload,
                    })
                    .await
                    .unwrap();
                }
                let pause = app
                    .build_queue_command(
                        QueueCommandType::CtrlSrvrSetPlayerState,
                        json!({"playing_state": 3}),
                    )
                    .await;
                let uuid = app
                    .send_queue_command(pause)
                    .await
                    .expect("pause must reach transport");
                app.clear_pending_if_matches(&uuid).await;
            }
            let sent = transport.sent_messages().await;
            assert_eq!(sent.len(), 5, "only the five user commands should be sent");
            assert!(sent
                .iter()
                .all(|message| message.message_type == "MESSAGE_TYPE_CTRL_SRVR_SET_PLAYER_STATE"));
            app.disconnect().await.unwrap();
        }

        #[tokio::test]
        async fn foreign_uuid_queue_snapshot_does_not_recursively_reask() {
            let (app, sink, transport, _events_rx) = build_connected_app().await;
            app.trigger_queue_state_resync().await;
            for _ in 0..5 {
                app.apply_server_event(QueueServerEvent {
                    event_type: QueueEventType::SrvrCtrlQueueState,
                    action_uuid: Some("00000000-0000-4000-8000-000000000001".into()),
                    queue_version: Some(QueueVersion::new(2, 1)),
                    payload: json!({"tracks": [], "shuffle_mode": false}),
                })
                .await
                .unwrap();
            }
            assert_eq!(
                transport.sent_messages().await.len(),
                1,
                "a snapshot fulfills a read, not a write conflict"
            );
            assert!(!sink.snapshot().await.iter().any(|event| matches!(
                event,
                QconnectAppEvent::PendingActionCanceledByConcurrentRemoteEvent { .. }
            )));
            app.disconnect().await.unwrap();
        }

        #[tokio::test]
        async fn background_queue_read_does_not_block_controller_intent() {
            for (command_type, payload) in [
                (
                    QueueCommandType::CtrlSrvrSetPlayerState,
                    json!({"playing_state": 3}),
                ),
                (
                    QueueCommandType::CtrlSrvrSetPlayerState,
                    json!({"playing_state": 2}),
                ),
                (
                    QueueCommandType::CtrlSrvrSetActiveRenderer,
                    json!({"renderer_id": 7}),
                ),
                (
                    QueueCommandType::CtrlSrvrAskForRendererState,
                    json!({"renderer_id": 7}),
                ),
                (
                    QueueCommandType::CtrlSrvrSetVolume,
                    json!({"renderer_id": 7, "volume": 50}),
                ),
                (
                    QueueCommandType::CtrlSrvrSetLoopMode,
                    json!({"loop_mode": 1}),
                ),
                (
                    QueueCommandType::CtrlSrvrMuteVolume,
                    json!({"renderer_id": 7, "value": false}),
                ),
            ] {
                let (app, _sink, transport, _events_rx) = build_connected_app().await;
                app.trigger_queue_state_resync().await;
                let query_uuid = app
                    .state_handle()
                    .lock()
                    .await
                    .pending
                    .current()
                    .unwrap()
                    .uuid
                    .clone();
                let command = app.build_queue_command(command_type, payload).await;
                app.send_queue_command(command)
                    .await
                    .expect("background reads must not starve user commands");
                assert_eq!(transport.sent_messages().await.len(), 2);
                assert_eq!(
                    app.state_handle()
                        .lock()
                        .await
                        .pending
                        .current()
                        .unwrap()
                        .uuid,
                    query_uuid
                );
                app.disconnect().await.unwrap();
            }
        }

        #[tokio::test(start_paused = true)]
        async fn unrelated_snapshots_do_not_renew_the_queue_read_timeout() {
            let (app, sink, transport, _events_rx) = build_connected_app().await;
            app.trigger_queue_state_resync().await;
            tokio::task::yield_now().await;
            let query_uuid = app
                .state_handle()
                .lock()
                .await
                .pending
                .current()
                .unwrap()
                .uuid
                .clone();
            for _ in 0..9 {
                tokio::time::advance(Duration::from_secs(1)).await;
                app.apply_server_event(QueueServerEvent {
                    event_type: QueueEventType::SrvrCtrlQueueState,
                    action_uuid: Some("00000000-0000-4000-8000-000000000001".into()),
                    queue_version: Some(QueueVersion::new(2, 1)),
                    payload: json!({"tracks": [], "shuffle_mode": false}),
                })
                .await
                .unwrap();
                assert_eq!(
                    app.state_handle()
                        .lock()
                        .await
                        .pending
                        .current()
                        .unwrap()
                        .uuid,
                    query_uuid
                );
            }
            tokio::time::advance(Duration::from_millis(1001)).await;
            tokio::task::yield_now().await;
            assert!(app.state_handle().lock().await.pending.current().is_none());
            assert_eq!(transport.sent_messages().await.len(), 1);
            assert!(sink.snapshot().await.iter().any(|event| matches!(event,
                QconnectAppEvent::PendingActionTimedOut { uuid, .. } if uuid == &query_uuid
            )));
            let load = app
                .build_queue_command(
                    QueueCommandType::CtrlSrvrQueueLoadTracks,
                    json!({"track_ids": [101]}),
                )
                .await;
            app.send_queue_command(load)
                .await
                .expect("a timed-out read must release queue writes");
            assert_eq!(transport.sent_messages().await.len(), 2);
            app.disconnect().await.unwrap();
        }

        #[tokio::test]
        async fn unrelated_snapshot_cannot_release_read_and_admit_a_write_before_its_reply() {
            let (app, _sink, transport, _events_rx) = build_connected_app().await;
            app.trigger_queue_state_resync().await;
            let query_uuid = app
                .state_handle()
                .lock()
                .await
                .pending
                .current()
                .unwrap()
                .uuid
                .clone();
            let snapshot = |action_uuid| QueueServerEvent {
                event_type: QueueEventType::SrvrCtrlQueueState,
                action_uuid,
                queue_version: Some(QueueVersion::new(2, 1)),
                payload: json!({"tracks": [], "shuffle_mode": false}),
            };
            app.apply_server_event(snapshot(Some(
                "00000000-0000-4000-8000-000000000001".into(),
            )))
            .await
            .unwrap();
            let load = app
                .build_queue_command(
                    QueueCommandType::CtrlSrvrQueueLoadTracks,
                    json!({"track_ids": [101]}),
                )
                .await;
            assert!(matches!(
                app.send_queue_command(load).await,
                Err(QconnectAppError::Pending(_))
            ));
            assert_eq!(
                transport.sent_messages().await.len(),
                1,
                "no write may overlap the outstanding read"
            );
            app.apply_server_event(snapshot(Some(query_uuid)))
                .await
                .unwrap();
            let load = app
                .build_queue_command(
                    QueueCommandType::CtrlSrvrQueueLoadTracks,
                    json!({"track_ids": [101]}),
                )
                .await;
            let load_uuid = app
                .send_queue_command(load)
                .await
                .expect("new queue can be sent after the read reply");
            let second_load = app
                .build_queue_command(
                    QueueCommandType::CtrlSrvrQueueLoadTracks,
                    json!({"track_ids": [102]}),
                )
                .await;
            assert!(matches!(
                app.send_queue_command(second_load).await,
                Err(QconnectAppError::Pending(_))
            ));
            assert_eq!(
                app.state_handle()
                    .lock()
                    .await
                    .pending
                    .current()
                    .unwrap()
                    .uuid,
                load_uuid
            );
            app.disconnect().await.unwrap();
        }

        #[tokio::test]
        async fn full_snapshot_recovers_write_conflict_without_querying_again() {
            let (app, sink, transport, _events_rx) = build_connected_app().await;
            let load = app
                .build_queue_command(
                    QueueCommandType::CtrlSrvrQueueLoadTracks,
                    json!({"track_ids": [101]}),
                )
                .await;
            app.send_queue_command(load).await.unwrap();
            app.apply_server_event(QueueServerEvent {
                event_type: QueueEventType::SrvrCtrlQueueState,
                action_uuid: Some("00000000-0000-4000-8000-000000000001".into()),
                queue_version: Some(QueueVersion::new(2, 1)),
                payload: json!({"tracks": [], "shuffle_mode": false}),
            })
            .await
            .unwrap();
            assert!(app.state_handle().lock().await.pending.current().is_none());
            assert_eq!(transport.sent_messages().await.len(), 1);
            assert!(sink.snapshot().await.iter().any(|event| matches!(
                event,
                QconnectAppEvent::PendingActionCanceledByConcurrentRemoteEvent { .. }
            )));
            app.disconnect().await.unwrap();
        }

        #[tokio::test]
        async fn shuffle_snapshot_fallback_is_only_for_incomplete_information() {
            for (version, payload, needs_snapshot) in [
                (
                    QueueVersion::new(1, 2),
                    json!({"shuffle_mode": true, "shuffle_seed": 0}),
                    false,
                ),
                (
                    QueueVersion::new(1, 2),
                    json!({"shuffle_mode": false}),
                    false,
                ),
                (QueueVersion::new(1, 2), json!({"shuffle_mode": true}), true),
                (
                    QueueVersion::new(1, 2),
                    json!({"shuffle_mode": true, "shuffle_seed": 123, "shuffle_pivot_queue_item_id": 99}),
                    true,
                ),
                (
                    QueueVersion::new(1, 3),
                    json!({"shuffle_mode": false}),
                    false,
                ),
            ] {
                let (app, _sink, transport, _events_rx) = build_connected_app().await;
                app.state_handle().lock().await.queue.version = QueueVersion::new(1, 1);
                app.apply_server_event(QueueServerEvent {
                    event_type: QueueEventType::SrvrCtrlShuffleModeSet,
                    action_uuid: None,
                    queue_version: Some(version),
                    payload,
                })
                .await
                .unwrap();
                assert_eq!(!transport.sent_messages().await.is_empty(), needs_snapshot);
                app.disconnect().await.unwrap();
            }
        }

        #[tokio::test]
        async fn failed_control_send_preserves_outstanding_queue_read() {
            let (app, _sink, transport, _events_rx) = build_connected_app().await;
            app.trigger_queue_state_resync().await;
            let query_uuid = app
                .state_handle()
                .lock()
                .await
                .pending
                .current()
                .unwrap()
                .uuid
                .clone();
            transport.disconnect().await.unwrap();
            let pause = app
                .build_queue_command(
                    QueueCommandType::CtrlSrvrSetPlayerState,
                    json!({"playing_state": 3}),
                )
                .await;
            assert!(matches!(
                app.send_queue_command(pause).await,
                Err(QconnectAppError::Transport(_))
            ));
            assert_eq!(
                app.state_handle()
                    .lock()
                    .await
                    .pending
                    .current()
                    .unwrap()
                    .uuid,
                query_uuid
            );
            app.abort_background_tasks().await;
        }

        #[tokio::test]
        async fn malformed_command_never_occupies_pending_slot() {
            let (app, _sink, transport, _events_rx) = build_connected_app().await;
            let command = QueueCommand::new(
                QueueCommandType::CtrlSrvrQueueLoadTracks,
                "not-a-uuid",
                QueueVersion::default(),
                json!({"track_ids": [101]}),
            );
            assert!(matches!(
                app.send_queue_command(command).await,
                Err(QconnectAppError::Protocol(_))
            ));
            assert!(app.state_handle().lock().await.pending.current().is_none());
            assert!(transport.sent_messages().await.is_empty());
            app.disconnect().await.unwrap();
        }

        #[tokio::test]
        async fn correlated_partial_and_session_events_do_not_complete_queue_read() {
            let (app, _sink, _transport, _events_rx) = build_connected_app().await;
            app.trigger_queue_state_resync().await;
            let query_uuid = app
                .state_handle()
                .lock()
                .await
                .pending
                .current()
                .unwrap()
                .uuid
                .clone();
            for (event_type, payload) in [
                (
                    QueueEventType::SrvrCtrlShuffleModeSet,
                    json!({"shuffle_mode": false}),
                ),
                (QueueEventType::SrvrCtrlLoopModeSet, json!({"loop_mode": 1})),
            ] {
                app.apply_server_event(QueueServerEvent {
                    event_type,
                    action_uuid: Some(query_uuid.clone()),
                    queue_version: Some(QueueVersion::default()),
                    payload,
                })
                .await
                .unwrap();
                assert_eq!(
                    app.state_handle()
                        .lock()
                        .await
                        .pending
                        .current()
                        .unwrap()
                        .uuid,
                    query_uuid
                );
            }
            app.disconnect().await.unwrap();
        }
    }

    #[tokio::test]
    async fn reorder_event_requests_authoritative_queue_state() {
        let (app, sink, transport, _events_rx) = build_connected_app().await;

        app.apply_server_event(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlQueueTracksReordered,
            action_uuid: Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string()),
            queue_version: Some(QueueVersion::new(2, 1)),
            payload: json!({
                "queue_item_ids": [3],
                "insert_after": 1,
                "autoplay_reset": false,
                "autoplay_loading": false
            }),
        })
        .await
        .expect("apply reorder event");

        let events = sink.snapshot().await;
        assert!(events
            .iter()
            .any(|event| matches!(event, QconnectAppEvent::QueueResyncTriggered)));

        let sent = transport.sent_messages().await;
        assert!(
            !sent.is_empty(),
            "expected ask-for-state resync after reorder"
        );
    }

    #[tokio::test]
    async fn remove_event_requests_authoritative_queue_state() {
        let (app, sink, transport, _events_rx) = build_connected_app().await;

        app.apply_server_event(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlQueueTracksRemoved,
            action_uuid: Some("b2c3d4e5-f6a7-8901-bcde-f12345678901".to_string()),
            queue_version: Some(QueueVersion::new(3, 1)),
            payload: json!({
                "queue_item_ids": [2],
                "autoplay_reset": false,
                "autoplay_loading": false
            }),
        })
        .await
        .expect("apply remove event");

        let events = sink.snapshot().await;
        assert!(events
            .iter()
            .any(|event| matches!(event, QconnectAppEvent::QueueResyncTriggered)));

        let sent = transport.sent_messages().await;
        assert!(
            !sent.is_empty(),
            "expected ask-for-state resync after remove"
        );
    }

    #[test]
    fn queue_hashes_diverge_is_inert_without_local_algorithm() {
        let mut queue = QConnectQueueState::default();
        queue.last_server_queue_hash = Some(vec![9, 9, 9]);
        assert!(
            !queue_hashes_diverge(&queue),
            "divergence detection must stay inert until the local hash algorithm is known"
        );
    }

    #[tokio::test]
    async fn queue_state_event_surfaces_server_queue_hash() {
        let (app, _sink, _transport, _events_rx) = build_connected_app().await;

        app.apply_server_event(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlQueueState,
            action_uuid: None,
            queue_version: Some(QueueVersion::new(1, 0)),
            payload: json!({
                "tracks": [],
                "shuffle_mode": false,
                "autoplay_tracks": [],
                "server_queue_hash": [1, 2, 3, 4]
            }),
        })
        .await
        .expect("apply queue-state event");

        let snap = app.queue_state_snapshot().await;
        assert_eq!(
            snap.last_server_queue_hash.as_deref(),
            Some(&[1u8, 2, 3, 4][..])
        );
    }

    #[tokio::test]
    async fn inbound_playback_error_emits_playback_error_app_event() {
        use prost::Message as _;
        use qconnect_protocol::{
            ErrorType, PlaybackErrorMessage, QConnectMessage, QConnectMessages, QueueVersionRef,
        };
        use qconnect_transport_ws::TransportEvent;

        let (app, sink, _transport, _events_rx) = build_connected_app().await;

        let batch = QConnectMessages {
            messages_time: None,
            messages_id: None,
            messages: vec![QConnectMessage {
                message_type: Some(2), // PLAYBACK_ERROR
                playback_error: Some(PlaybackErrorMessage {
                    queue_version: Some(QueueVersionRef {
                        major: Some(4),
                        minor: Some(2),
                    }),
                    queue_item_id: Some(77),
                    error_type: Some(5), // NETWORK_ERROR
                }),
                ..Default::default()
            }],
        };

        app.handle_transport_event(TransportEvent::InboundPayloadBytes {
            cloud_message_type: 0,
            payload: batch.encode_to_vec(),
        })
        .await
        .expect("handle playback-error frame");

        let events = sink.snapshot().await;
        assert!(
            events.iter().any(|event| matches!(
                event,
                QconnectAppEvent::PlaybackError {
                    queue_item_id: 77,
                    error_type: ErrorType::NetworkError,
                    ..
                }
            )),
            "expected a PlaybackError app event for queue item 77 / NetworkError"
        );
    }

    #[tokio::test]
    async fn autoplay_growth_triggers_queue_state_resync() {
        let (app, sink, transport, _events_rx) = build_connected_app().await;

        app.apply_server_event(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlQueueTracksAddedFromAutoplay,
            action_uuid: None,
            queue_version: Some(QueueVersion::new(2, 0)),
            payload: json!({ "queue_item_ids": [101, 102] }),
        })
        .await
        .expect("apply autoplay-growth event");

        let events = sink.snapshot().await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, QconnectAppEvent::QueueResyncTriggered)),
            "autoplay growth must force an AskForQueueState resync"
        );

        let sent = transport.sent_messages().await;
        assert!(
            !sent.is_empty(),
            "expected ask-for-state resync after autoplay growth"
        );
    }

    #[tokio::test]
    async fn late_queue_error_for_concurrency_canceled_action_is_ignored() {
        let (app, sink, _, _events_rx) = build_connected_app().await;
        {
            let state = app.state_handle();
            let mut guard = state.lock().await;
            guard.concurrency_canceled_action_uuid =
                Some("85fa0dd6-7bd6-4b3c-8f43-b8ee22e65d5e".to_string());
        }

        let late_error = QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlQueueErrorMessage,
            action_uuid: Some("85fa0dd6-7bd6-4b3c-8f43-b8ee22e65d5e".to_string()),
            queue_version: Some(QueueVersion::new(2, 1)),
            payload: json!({
                "error_code": "409",
                "error_message": "stale_queue_version"
            }),
        };

        app.apply_server_event(late_error)
            .await
            .expect("apply late queue error");

        let events = sink.snapshot().await;
        assert!(events.iter().any(|event| {
            matches!(
                event,
                QconnectAppEvent::QueueErrorIgnoredByConcurrency { action_uuid }
                if action_uuid == "85fa0dd6-7bd6-4b3c-8f43-b8ee22e65d5e"
            )
        }));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, QconnectAppEvent::QueueUpdated(_))),
            "ignored late queue error should not mutate queue"
        );
    }

    /// The server can reject QUEUE_LOAD_TRACKS with no action UUID. In that
    /// form correlation cannot match by UUID, but retaining the rejected load
    /// until the generic timeout blocks the authoritative local-queue retry.
    #[tokio::test]
    async fn queue_load_error_without_action_uuid_releases_rejected_pending_action() {
        let (app, sink, _transport, _events_rx) = build_connected_app().await;
        let command = app
            .build_queue_command(
                QueueCommandType::CtrlSrvrQueueLoadTracks,
                json!({ "track_ids": [101, 102] }),
            )
            .await;
        let rejected_uuid = app
            .send_queue_command(command)
            .await
            .expect("send queue-load command");

        app.apply_server_event(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlQueueErrorMessage,
            action_uuid: None,
            queue_version: Some(QueueVersion::new(2, 1)),
            payload: json!({
                "error_code": "ERROR_QUEUE_LOAD_TRACKS",
                "error_message": "Queue version mismatch"
            }),
        })
        .await
        .expect("apply queue-load rejection without action UUID");

        let state = app.state_handle();
        let guard = state.lock().await;
        let pending = guard
            .pending
            .current()
            .expect("queue-state resync should replace the rejected action");
        assert_ne!(pending.uuid, rejected_uuid);
        assert!(pending.is_ask_for_state_action);
        assert!(!pending.is_queue_load_tracks_action);
        drop(guard);

        let events = sink.snapshot().await;
        assert!(events.iter().any(|event| {
            matches!(
                event,
                QconnectAppEvent::PendingActionCanceledByConcurrentRemoteEvent {
                    pending_uuid,
                    remote_action_uuid,
                } if pending_uuid == &rejected_uuid && remote_action_uuid.is_empty()
            )
        }));
        assert!(events
            .iter()
            .any(|event| matches!(event, QconnectAppEvent::QueueResyncTriggered)));
    }

    #[tokio::test]
    async fn matching_session_management_event_completes_pending_action() {
        let (app, sink, _transport, _events_rx) = build_connected_app().await;
        let command = app
            .build_queue_command(
                QueueCommandType::CtrlSrvrSetActiveRenderer,
                json!({ "active_renderer_id": 1 }),
            )
            .await;
        let pending_uuid = app
            .send_queue_command(command)
            .await
            .expect("send set-active-renderer command");

        let matching_event = QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlActiveRendererChanged,
            action_uuid: Some(pending_uuid.clone()),
            queue_version: Some(QueueVersion::new(1, 1)),
            payload: json!({ "active_renderer_id": 1 }),
        };

        app.apply_server_event(matching_event)
            .await
            .expect("apply active-renderer-changed event");

        let second_command = app
            .build_queue_command(
                QueueCommandType::CtrlSrvrQueueLoadTracks,
                json!({ "track_ids": [101, 102] }),
            )
            .await;

        app.send_queue_command(second_command)
            .await
            .expect("pending action should be cleared by matching session event");

        let events = sink.snapshot().await;
        assert!(events.iter().any(|event| {
            matches!(
                event,
                QconnectAppEvent::PendingActionCompleted { uuid } if uuid == &pending_uuid
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                QconnectAppEvent::SessionManagementEvent { message_type, .. }
                if message_type == "MESSAGE_TYPE_SRVR_CTRL_ACTIVE_RENDERER_CHANGED"
            )
        }));
    }

    #[tokio::test]
    async fn active_renderer_changed_without_action_uuid_completes_matching_pending_action() {
        let (app, sink, _transport, _events_rx) = build_connected_app().await;
        let command = app
            .build_queue_command(
                QueueCommandType::CtrlSrvrSetActiveRenderer,
                json!({ "renderer_id": 16 }),
            )
            .await;
        let pending_uuid = app
            .send_queue_command(command)
            .await
            .expect("send set-active-renderer command");

        app.apply_server_event(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlActiveRendererChanged,
            action_uuid: None,
            queue_version: Some(QueueVersion::new(1, 1)),
            payload: json!({ "active_renderer_id": 16 }),
        })
        .await
        .expect("apply active-renderer-changed event without action uuid");

        let second_command = app
            .build_queue_command(
                QueueCommandType::CtrlSrvrQueueLoadTracks,
                json!({ "track_ids": [201, 202] }),
            )
            .await;

        app.send_queue_command(second_command)
            .await
            .expect("matching active-renderer change should clear pending action");

        let events = sink.snapshot().await;
        assert!(events.iter().any(|event| {
            matches!(
                event,
                QconnectAppEvent::PendingActionCompleted { uuid } if uuid == &pending_uuid
            )
        }));
    }

    #[tokio::test]
    async fn renderer_state_update_completes_transport_control_pending_without_action_uuid() {
        let (app, sink, _transport, _events_rx) = build_connected_app().await;
        let command = app
            .build_queue_command(
                QueueCommandType::CtrlSrvrSetPlayerState,
                json!({
                    "playing_state": 2,
                    "current_position": 0,
                    "current_queue_item": {
                        "queue_version": { "major": 1, "minor": 1 },
                        "id": 1
                    }
                }),
            )
            .await;
        let pending_uuid = app
            .send_queue_command(command)
            .await
            .expect("send set-player-state command");

        app.apply_server_event(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlRendererStateUpdated,
            action_uuid: None,
            queue_version: Some(QueueVersion::new(1, 1)),
            payload: json!({
                "renderer_id": 1,
                "status": 1,
                "player_state": {
                    "playing_state": 2,
                    "current_position": 1234,
                    "current_queue_item_id": 1
                }
            }),
        })
        .await
        .expect("apply renderer-state-updated event");

        let second_command = app
            .build_queue_command(
                QueueCommandType::CtrlSrvrSetPlayerState,
                json!({
                    "playing_state": 2,
                    "current_position": 0,
                    "current_queue_item": {
                        "queue_version": { "major": 1, "minor": 1 },
                        "id": 2
                    }
                }),
            )
            .await;

        app.send_queue_command(second_command)
            .await
            .expect("transport control pending should clear after renderer-state update");

        let events = sink.snapshot().await;
        assert!(events.iter().any(|event| {
            matches!(
                event,
                QconnectAppEvent::PendingActionCompleted { uuid } if uuid == &pending_uuid
            )
        }));
    }

    #[tokio::test]
    async fn loop_mode_set_without_action_uuid_completes_matching_pending_action() {
        let (app, sink, _transport, _events_rx) = build_connected_app().await;
        let command = app
            .build_queue_command(
                QueueCommandType::CtrlSrvrSetLoopMode,
                json!({ "loop_mode": 3 }),
            )
            .await;
        let pending_uuid = app
            .send_queue_command(command)
            .await
            .expect("send set-loop-mode command");

        app.apply_server_event(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlLoopModeSet,
            action_uuid: None,
            queue_version: Some(QueueVersion::new(1, 1)),
            payload: json!({ "loop_mode": 3 }),
        })
        .await
        .expect("apply loop-mode-set event without action uuid");

        let second_command = app
            .build_queue_command(
                QueueCommandType::CtrlSrvrSetLoopMode,
                json!({ "loop_mode": 2 }),
            )
            .await;

        app.send_queue_command(second_command)
            .await
            .expect("loop-mode-set event should clear pending action");

        let events = sink.snapshot().await;
        assert!(events.iter().any(|event| {
            matches!(
                event,
                QconnectAppEvent::PendingActionCompleted { uuid } if uuid == &pending_uuid
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                QconnectAppEvent::SessionManagementEvent { message_type, .. }
                if message_type == "MESSAGE_TYPE_SRVR_CTRL_LOOP_MODE_SET"
            )
        }));
    }

    #[tokio::test]
    async fn play_next_and_later_batches_keep_the_complete_manual_block() {
        let (app, sink, _, _events_rx) = build_connected_app().await;
        *sink.observed.lock().unwrap() = Some(qbz_player::player::PlaybackEvent {
            track_id: 1,
            position: 67,
            is_playing: true,
            buffer_state: PlaybackBufferState::Ready,
            ..Default::default()
        });
        let mut base = vec![1, 2, 3];
        for target in [vec![1, 10, 11, 2, 3], vec![1, 10, 11, 12, 13, 2, 3]] {
            {
                let mut state = app.state.lock().await;
                state.queue.queue_items = base
                    .iter()
                    .enumerate()
                    .map(|(i, id)| qconnect_core::QueueItem {
                        track_context_uuid: String::new(),
                        track_id: *id,
                        queue_item_id: i as u64,
                    })
                    .collect();
                state.renderer.current_track = Some(state.queue.queue_items[0].clone());
            }
            let command = app.build_local_queue_sync_command(&target, 0, true).await;
            assert_eq!(
                command.command_type,
                QueueCommandType::CtrlSrvrQueueLoadTracks
            );
            assert_eq!(command.payload["track_ids"], json!(target));
            assert_eq!(command.payload["queue_position"], 0);
            base = target;
        }
    }

    #[tokio::test]
    async fn local_append_preserves_clock_and_duplicate_occurrences() {
        let (app, sink, _, _) = build_connected_app().await;
        *sink.observed.lock().unwrap() = Some(qbz_player::player::PlaybackEvent {
            track_id: 10,
            position: 67,
            is_playing: true,
            buffer_state: PlaybackBufferState::Ready,
            ..Default::default()
        });
        {
            let mut state = app.state.lock().await;
            state.queue.version = QueueVersion::new(8, 1);
            state.queue.queue_items = [10, 20, 10]
                .into_iter()
                .enumerate()
                .map(|(i, id)| qconnect_core::QueueItem {
                    track_context_uuid: String::new(),
                    track_id: id,
                    queue_item_id: i as u64,
                })
                .collect();
            state.renderer.current_track = Some(state.queue.queue_items[2].clone());
        }
        let append = app
            .build_local_queue_sync_command(&[10, 20, 10, 30, 30], 2, true)
            .await;
        assert_eq!(
            append.command_type,
            QueueCommandType::CtrlSrvrQueueAddTracks
        );
        assert_eq!(append.payload["track_ids"], json!([30, 30]));
        assert_eq!(append.payload["autoplay_reset"], false);
        assert!(append.payload.get("queue_position").is_none());
        assert!(append.payload.get("shuffle_mode").is_none());
        assert_eq!(append.queue_version_ref, QueueVersion::new(8, 1));
        for (ids, current, allowed) in [
            // Indistinguishable from inserting another 10 before the old one.
            (vec![10, 20, 10, 10, 30], 2, true),
            (vec![10, 20, 10, 30], 0, true),
            (vec![20, 10, 10, 30], 2, true),
            (vec![10, 20, 10, 30], 2, false),
        ] {
            assert_eq!(
                app.build_local_queue_sync_command(&ids, current, allowed)
                    .await
                    .command_type,
                QueueCommandType::CtrlSrvrQueueLoadTracks
            );
        }
        // In shuffle, a canonical tail insertion could mean "play next" in
        // playback order. Never infer append from catalog order alone.
        app.state.lock().await.queue.shuffle_mode = true;
        assert_eq!(
            app.build_local_queue_sync_command(&[10, 20, 10, 30], 2, true)
                .await
                .command_type,
            QueueCommandType::CtrlSrvrQueueLoadTracks
        );
        app.state.lock().await.queue.shuffle_mode = false;
        sink.observed.lock().unwrap().as_mut().unwrap().buffer_state =
            PlaybackBufferState::InitialBuffering;
        assert_eq!(
            app.build_local_queue_sync_command(&[10, 20, 10, 30], 2, true)
                .await
                .command_type,
            QueueCommandType::CtrlSrvrQueueLoadTracks
        );
    }

    #[tokio::test]
    async fn queue_echo_keeps_live_position_in_reducer_and_controller_report() {
        let (app, sink, transport, _events_rx) = build_connected_app().await;
        *sink.observed.lock().unwrap() = Some(qbz_player::player::PlaybackEvent {
            track_id: 386331742,
            position: 67,
            duration: 290,
            is_playing: true,
            buffer_state: qbz_player::player::PlaybackBufferState::Ready,
            ..Default::default()
        });
        app.state.lock().await.queue.queue_items = vec![qconnect_core::QueueItem {
            track_context_uuid: "queue-edit".into(), track_id: 386331742, queue_item_id: 0,
        }];
        app.apply_renderer_server_command(RendererServerCommand {
            command_type: RendererCommandType::SrvrRndrSetState,
            payload: json!({ "playing_state": 2, "current_position": 0,
                "current_track": {"track_context_uuid": "queue-edit", "track_id": 386331742, "queue_item_id": 0} }),
        }).await.unwrap();
        assert_eq!(
            app.renderer_state_snapshot().await.current_position_ms,
            Some(67000)
        );
        let messages = transport.sent_messages().await;
        let report = messages
            .iter()
            .find(|message| message.message_type == "MESSAGE_TYPE_RNDR_SRVR_STATE_UPDATED")
            .unwrap();
        assert_eq!(report.payload["current_position"], 67000);
        assert_eq!(report.payload["buffer_state"], 2);
        assert_eq!(report.payload["playing_state"], 2);
    }

    #[tokio::test]
    async fn locked_renderer_ignores_volume_and_mute_before_execution_or_reporting() {
        let (app, sink, transport, _events_rx) = build_connected_app().await;
        {
            let mut state = app.state.lock().await;
            state.renderer.volume = Some(100);
            state.renderer.muted = Some(false);
        }
        let before = app.renderer_state_snapshot().await;
        let sent = transport.sent_messages().await.len();
        let events = sink.snapshot().await.len();
        sink.volume_locked.store(true, Ordering::SeqCst);
        for (command_type, payload) in [
            (RendererCommandType::SrvrRndrSetVolume, json!({"volume": 20})),
            (RendererCommandType::SrvrRndrSetVolume, json!({"volume_delta": -5})),
            (RendererCommandType::SrvrRndrSetVolume, json!({"volume_delta": 5})),
            (RendererCommandType::SrvrRndrMuteVolume, json!({"value": true})),
            (RendererCommandType::SrvrRndrMuteVolume, json!({"value": false})),
        ] {
            app.apply_renderer_server_command(RendererServerCommand { command_type, payload })
                .await.expect("unsupported volume is ignored without a renderer error");
            assert_eq!(app.renderer_state_snapshot().await, before);
            assert_eq!(transport.sent_messages().await.len(), sent);
            assert_eq!(sink.snapshot().await.len(), events);
        }
        // The policy is live: unlocking the same renderer admits the next command.
        sink.volume_locked.store(false, Ordering::SeqCst);
        app.apply_renderer_server_command(RendererServerCommand {
            command_type: RendererCommandType::SrvrRndrSetVolume,
            payload: json!({"volume": 42}),
        }).await.unwrap();
        assert_eq!(app.renderer_state_snapshot().await.volume, Some(42));
        assert_eq!(transport.sent_messages().await.len(), sent + 1);
        assert!(sink.snapshot().await.len() > events);
    }

    #[tokio::test]
    async fn inbound_renderer_command_updates_renderer_state() {
        let (app, sink, transport, _events_rx) = build_connected_app().await;

        app.state.lock().await.queue.queue_items = vec![qconnect_core::QueueItem {
            track_context_uuid: "ctx-remote".into(), track_id: 777001, queue_item_id: 991,
        }];
        app.apply_renderer_server_command(RendererServerCommand {
            command_type: RendererCommandType::SrvrRndrSetState,
            payload: json!({
                "playing_state": 2,
                "current_position": 65321,
                "current_track": {
                    "track_context_uuid": "ctx-remote",
                    "track_id": 777001,
                    "queue_item_id": 991
                }
            }),
        })
        .await
        .expect("apply set-state command");

        app.apply_renderer_server_command(RendererServerCommand {
            command_type: RendererCommandType::SrvrRndrSetVolume,
            payload: json!({
                "volume": 52,
                "volume_delta": 3
            }),
        })
        .await
        .expect("apply set-volume command");

        let renderer = app.renderer_state_snapshot().await;
        assert_eq!(renderer.playing_state, Some(2));
        assert_eq!(renderer.current_position_ms, Some(65_321));
        assert_eq!(renderer.volume, Some(55));
        assert_eq!(renderer.volume_delta, Some(3));
        assert_eq!(
            renderer
                .current_track
                .as_ref()
                .map(|item| item.queue_item_id),
            Some(991)
        );

        let events = sink.snapshot().await;
        assert!(
            events
                .iter()
                .filter(|event| matches!(event, QconnectAppEvent::RendererUpdated(_)))
                .count()
                >= 2
        );
        assert!(events
            .iter()
            .any(|event| matches!(event, QconnectAppEvent::RendererCommandApplied { .. })));

        let sent = transport.sent_messages().await;
        assert!(sent
            .iter()
            .any(|msg| msg.message_type == "MESSAGE_TYPE_RNDR_SRVR_STATE_UPDATED"));
        assert!(sent
            .iter()
            .any(|msg| msg.message_type == "MESSAGE_TYPE_RNDR_SRVR_VOLUME_CHANGED"));

        let state_update = sent
            .iter()
            .find(|msg| msg.message_type == "MESSAGE_TYPE_RNDR_SRVR_STATE_UPDATED")
            .expect("state update report");
        assert!(state_update
            .payload
            .get("current_queue_item_id")
            .expect("current_queue_item_id field")
            .is_null());
        assert!(state_update
            .payload
            .get("next_queue_item_id")
            .expect("next_queue_item_id field")
            .is_null());
    }

    #[tokio::test]
    async fn handoff_execution_failure_cannot_publish_intent_as_success() {
        let (app, sink, transport, _events_rx) = build_connected_app().await;
        sink.fail_execution.store(true, Ordering::SeqCst);
        let before = app.renderer_state_snapshot().await;
        let sent = transport.sent_messages().await.len();
        let events = sink.snapshot().await.len();
        assert!(app.apply_renderer_server_command(RendererServerCommand {
            command_type: RendererCommandType::SrvrRndrSetState,
            payload: json!({"playing_state": 3}),
        }).await.is_err());
        assert_eq!(app.renderer_state_snapshot().await, before);
        assert_eq!(transport.sent_messages().await.len(), sent);
        assert_eq!(sink.snapshot().await.len(), events);
        sink.fail_execution.store(false, Ordering::SeqCst);
        app.apply_renderer_server_command(RendererServerCommand {
            command_type: RendererCommandType::SrvrRndrSetState,
            payload: json!({"playing_state": 3}),
        }).await.unwrap();
        assert_eq!(app.renderer_state_snapshot().await.playing_state, Some(3));
    }

    #[tokio::test]
    async fn handoff_incompatible_queue_is_resynced_before_any_renderer_effect() {
        let (app, sink, transport, _events_rx) = build_connected_app().await;
        app.state.lock().await.queue.version = qconnect_core::QueueVersion::new(5, 1);
        let before = app.renderer_state_snapshot().await;
        app.apply_renderer_server_command(RendererServerCommand {
            command_type: RendererCommandType::SrvrRndrSetState,
            payload: json!({"playing_state": 2, "current_position": 0,
                "queue_version": {"major": 4, "minor": 9},
                "current_track": {"track_id": 10, "queue_item_id": 0}}),
        }).await.unwrap();
        assert_eq!(app.renderer_state_snapshot().await, before);
        assert!(sink.snapshot().await.iter().all(|event| !matches!(event,
            QconnectAppEvent::RendererUpdated(_) | QconnectAppEvent::RendererCommandApplied { .. })));
        assert!(transport.sent_messages().await.iter().all(|message|
            message.message_type != "MESSAGE_TYPE_RNDR_SRVR_STATE_UPDATED"));
        assert!(app.state.lock().await.deferred_renderer_command.is_some());
        app.apply_renderer_server_command(RendererServerCommand {
            command_type: RendererCommandType::SrvrRndrSetState, payload: json!({"playing_state": 1, "queue_version": {"major": 4, "minor": 9}}),
        }).await.unwrap();
        assert!(app.state.lock().await.deferred_renderer_command.is_none());
    }

    #[tokio::test]
    async fn handoff_deferred_handoff_replays_the_latest_pause_and_position_after_resync() {
        let (app, sink, _transport, _events_rx) = build_connected_app().await;
        let command = |payload| RendererServerCommand {
            command_type: RendererCommandType::SrvrRndrSetState, payload,
        };
        app.apply_renderer_server_command(command(json!({
            "playing_state": 2, "current_position": 0,
            "queue_version": {"major": 9, "minor": 0},
            "current_track": {"track_id": 10, "queue_item_id": 0}
        }))).await.unwrap();
        app.apply_renderer_server_command(command(json!({"playing_state": 3, "queue_version": {"major": 9, "minor": 0}}))).await.unwrap();
        app.apply_renderer_server_command(command(json!({"current_position": 45_000}))).await.unwrap();
        let before = sink.snapshot().await.len();
        app.apply_server_event(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlQueueState,
            action_uuid: None,
            queue_version: Some(QueueVersion::new(9, 1)),
            payload: json!({"tracks": [{"track_id": 10, "queue_item_id": 0}], "shuffle_mode": false}),
        }).await.unwrap();
        let renderer = app.renderer_state_snapshot().await;
        assert_eq!(renderer.playing_state, Some(3));
        assert_eq!(renderer.current_position_ms, Some(45_000));
        assert_eq!(renderer.current_track.unwrap().queue_item_id, 0);
        assert!(app.state.lock().await.deferred_renderer_command.is_none());
        assert!(sink.snapshot().await[before..].iter().any(|event| matches!(event,
            QconnectAppEvent::RendererCommandApplied {
                command: qconnect_core::RendererCommand::SetState { playing_state: Some(3), .. }, .. })));
    }

    #[tokio::test]
    async fn handoff_retired_activation_cannot_replay_a_deferred_target() {
        let (app, _sink, _transport, _events_rx) = build_connected_app().await;
        app.apply_renderer_server_command(RendererServerCommand {
            command_type: RendererCommandType::SrvrRndrSetState,
            payload: json!({"playing_state": 2, "current_track": {"track_id": 10, "queue_item_id": 0}}),
        }).await.unwrap();
        app.sync.lock().await.renderer_generation += 1;
        app.apply_server_event(QueueServerEvent {
            event_type: QueueEventType::SrvrCtrlQueueState, action_uuid: None,
            queue_version: Some(QueueVersion::new(1, 1)),
            payload: json!({"tracks": [{"track_id": 10, "queue_item_id": 0}], "shuffle_mode": false}),
        }).await.unwrap();
        assert!(app.renderer_state_snapshot().await.current_track.is_none());
    }

    #[tokio::test]
    async fn local_authority_fence_blocks_renderer_reducer_and_reports() {
        let (app, sink, transport, _events_rx) = build_connected_app().await;
        {
            let handle = app.sync_handle();
            let mut state = handle.lock().await;
            crate::arm_local_queue_takeover(&mut state, vec![10, 20], "load-local".to_string());
        }

        app.apply_renderer_server_command(RendererServerCommand {
            command_type: RendererCommandType::SrvrRndrSetState,
            payload: json!({
                "playing_state": 3,
                "current_position": 46_870,
                "current_track": {
                    "track_context_uuid": "stale",
                    "track_id": 439_248_309_u64,
                    "queue_item_id": 0,
                }
            }),
        })
        .await
        .expect("fenced renderer command");

        let renderer = app.renderer_state_snapshot().await;
        assert_eq!(renderer.playing_state, None);
        assert_eq!(renderer.current_position_ms, None);
        assert!(sink.snapshot().await.iter().all(|event| !matches!(
            event,
            QconnectAppEvent::RendererUpdated(_) | QconnectAppEvent::RendererCommandApplied { .. }
        )));
        assert!(transport.sent_messages().await.is_empty());
    }
}
