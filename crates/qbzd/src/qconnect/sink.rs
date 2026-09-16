// TODO(converge: qconnect-glue) — copied from crates/qbz/src/qconnect_event_sink.rs @ c8ef2a1b;
// do not fix bugs here without fixing the source, and vice versa.
//
//! Daemon `QconnectEventSink`.
//!
//! Receives `QconnectAppEvent`s from the qconnect-app crate and dispatches the
//! renderer-critical arms into the [`DaemonRendererEngine`] (the renderer seam),
//! the shared `QconnectRemoteSyncState` accumulator, and the frontend-agnostic
//! `qconnect_app::renderer` orchestration (materialize / apply / loop-mode /
//! cursor-align). The session-management critical section is delegated to
//! `QconnectApp::apply_session_management_event`; only the post-lock work the
//! returned `SessionApplyOutcome` asks for runs here.
//!
//! Daemon adaptation vs. the Slint copy (§1.4): the FOUR renderer-critical arms
//! (QueueUpdated / RendererCommandApplied / SessionManagementEvent /
//! RendererUpdated) are wired so the daemon functions AS a renderer; every UI arm
//! (device picker, now-playing card, DEV modal, toasts) is dropped, and the three
//! desktop `toast::error_weak` surfaces become `log::warn!`.

use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use qconnect_app::{
    active_peer_renderer_is_playing, build_session_renderer_snapshot, cache_renderer_snapshot,
    is_peer_renderer_active, remote_renderer_commands_are_fenced, should_materialize_remote_queue,
    QconnectApp, QconnectAppEvent, QconnectEventSink, QconnectRemoteSyncState,
    QconnectRendererEngine, RendererCommand,
};
use qconnect_transport_ws::NativeWsTransport;
use serde_json::Value;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::authority::{AuthorityCell, AuthorityStamp};
use super::engine::DaemonRendererEngine;
use super::lan::DaemonLanProjectionSlot;
use super::transport::resolve_local_identity;

/// Concrete `QconnectApp` type used by the daemon adapter.
pub type DaemonQconnectApp = QconnectApp<NativeWsTransport, DaemonEventSink>;

pub struct DaemonEventSink {
    /// Renderer seam — forwards the `qconnect_app::renderer` orchestration onto
    /// `runtime.core()` + the protected player.
    engine: DaemonRendererEngine,
    /// THE shared remote-sync accumulator (one Mutex, shared with `QconnectApp`).
    sync_state: Arc<Mutex<QconnectRemoteSyncState>>,
    /// Exact runtime authority. The sink is constructed while a runtime is
    /// prepared, and remains dormant until this stamp is installed.
    authority: Arc<AuthorityCell>,
    stamp: AuthorityStamp,
    /// Public LAN session projection. Updates are serialized with authority
    /// installs so late SESSION_STATE events cannot overwrite a replacement.
    projection: DaemonLanProjectionSlot,
    /// Late-bound weak handle to the owning app, wired via `set_app` after the
    /// app is built FROM this sink. Used to emit renderer reports (e.g.
    /// is_active=true after SetActive(true)) and to drive the session-apply +
    /// freeze/watchdog without an ownership cycle.
    app: Arc<OnceLock<Weak<DaemonQconnectApp>>>,
    /// FIX #13: previous "a peer is the active renderer" state, tracked across
    /// `apply_session_management_event` calls. On a false->true transition (the
    /// daemon becomes a CONTROLLER) we fire one `ask_for_active_renderer_state` to
    /// fetch the peer's full state (incl. `current_queue_item_id`) so the queue
    /// cursor resolves the peer's CURRENT track immediately instead of staying
    /// stale until the peer changes track. Edge-detected to avoid spamming on
    /// every periodic state-update frame.
    last_peer_active: std::sync::atomic::AtomicBool,
}

impl DaemonEventSink {
    pub fn volume_mode(&self) -> super::engine::VolumeMode {
        self.engine.volume_mode()
    }

    pub fn new(
        engine: DaemonRendererEngine,
        sync_state: Arc<Mutex<QconnectRemoteSyncState>>,
        authority: Arc<AuthorityCell>,
        stamp: AuthorityStamp,
        projection: DaemonLanProjectionSlot,
    ) -> Self {
        Self {
            engine,
            sync_state,
            authority,
            stamp,
            projection,
            app: Arc::new(OnceLock::new()),
            last_peer_active: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Wire the owning app after construction. Idempotent (OnceLock).
    pub fn set_app(&self, app: &Arc<DaemonQconnectApp>) {
        let _ = self.app.set(Arc::downgrade(app));
    }

    fn is_current(&self) -> bool {
        self.authority.is_current(self.stamp)
    }

    /// Acknowledge activation with the actual player state, including buffering.
    async fn report_active_renderer_ready(&self) {
        if !self.is_current() {
            return;
        }
        let Some(app) = self.app.get().and_then(Weak::upgrade) else {
            return;
        };
        let queue = app.queue_state_snapshot().await;
        let renderer = app.renderer_state_snapshot().await;
        if !self.is_current() {
            return;
        }
        let event = self.engine.playback_event();
        let snapshot = qconnect_app::playback_snapshot_from_event(
            &event,
            &queue,
            renderer
                .current_track
                .as_ref()
                .map(|item| item.queue_item_id),
            renderer.next_track.as_ref().map(|item| item.queue_item_id),
        );
        let report = qconnect_app::build_renderer_playback_report(
            Uuid::new_v4().to_string(),
            queue.version,
            snapshot,
        );
        if !self.is_current() {
            return;
        }
        if let Err(err) = app.send_renderer_report_command(report).await {
            if !self.is_current() {
                return;
            }
            log::warn!("[QConnect] Failed to report active-renderer-ready: {err}");
        }
    }

    /// Apply a server session-management event by delegating the locked critical
    /// section to qconnect-app, then running the post-lock renderer-engine work
    /// the returned `SessionApplyOutcome` asks for. Mirrors the Tauri
    /// `apply_session_management_event`; the post-lock ordering (loop mode ->
    /// local-playback handoff -> projection -> freeze -> watchdog) is identical.
    async fn apply_session_management_event(&self, message_type: &str, payload: &Value) {
        if !self.is_current() {
            return;
        }
        let Some(app) = self.app.get().and_then(Weak::upgrade) else {
            return;
        };
        let identity = resolve_local_identity();
        let outcome = app
            .apply_session_management_event(message_type, payload, &identity)
            .await;
        if !self.is_current() {
            return;
        }

        if message_type == "MESSAGE_TYPE_SRVR_CTRL_SESSION_STATE" {
            let session_id = {
                let state = self.sync_state.lock().await;
                state.session.session_uuid.clone()
            };
            if !self.is_current() {
                return;
            }
            if let Some(session_id) = session_id {
                self.projection
                    .confirm_owner_session(&self.authority, self.stamp, &session_id);
            }
        }

        if let Some(loop_mode) = outcome.apply_loop_mode {
            if !self.is_current() {
                return;
            }
            if let Err(err) =
                qconnect_app::renderer::apply_remote_loop_mode(&self.engine, loop_mode).await
            {
                if !self.is_current() {
                    return;
                }
                log::warn!("[QConnect] Failed to apply remote loop mode: {err}");
            }
        }

        if outcome.sync_local_playback {
            self.sync_local_playback_for_renderer_ownership().await;
        }

        if let Some(renderer_id) = outcome.remote_projection_renderer_id {
            self.sync_active_renderer_projection(renderer_id).await;
        }

        if let Some(renderer_id) = outcome.disconnected_renderer_id {
            if !self.is_current() {
                return;
            }
            app.freeze_active_renderer_projection(
                renderer_id,
                QconnectAppEvent::RendererDisconnected { renderer_id },
            )
            .await;
            if !self.is_current() {
                return;
            }
        }

        if let Some((renderer_id, generation)) = outcome.watchdog_arm {
            if !self.is_current() {
                return;
            }
            app.arm_renderer_watchdog(renderer_id, generation);
        }

        // FIX #13: when the daemon transitions INTO controller mode (a PEER
        // becomes the active renderer), the peer's periodic state-update frames
        // carry `current_queue_item_id: null` (position-only), so on the
        // transition the cursor/projection can't resolve the peer's CURRENT track.
        // Fetch the peer's FULL state once on the false->true edge so the existing
        // align + projection resolve the real current track now.
        let peer_active_now = {
            let state = self.sync_state.lock().await;
            if !self.is_current() {
                return;
            }
            is_peer_renderer_active(&state.session)
        };
        let was_peer_active = self
            .last_peer_active
            .swap(peer_active_now, std::sync::atomic::Ordering::Relaxed);
        let conflict_pending = {
            let state = self.sync_state.lock().await;
            state.local_playback_conflict_pending
        };
        if peer_active_now && !was_peer_active && !conflict_pending {
            if !self.is_current() {
                return;
            }
            if let Err(err) = app.ask_for_active_renderer_state().await {
                if !self.is_current() {
                    return;
                }
                log::warn!(
                    "[QConnect] controller entry: ask_for_active_renderer_state failed: {err}"
                );
            }
        }
    }

    /// When the active PEER renderer is actually playing, stop our local playback
    /// so the two don't double-play. A paused/stale peer must not interrupt QBZ.
    async fn sync_local_playback_for_renderer_ownership(&self) {
        if !self.is_current() {
            return;
        }
        let peer_renderer_playing = {
            let state = self.sync_state.lock().await;
            if !self.is_current() {
                return;
            }
            if state.local_playback_conflict_pending {
                false
            } else {
                active_peer_renderer_is_playing(&state)
            }
        };
        if !peer_renderer_playing {
            return;
        }

        let playback_state = self.engine.get_playback_state();
        // stop() intentionally preserves current_track_id, so track_id alone
        // would issue another stop for every peer state/volume/quality event.
        if playback_state.track_id == 0 || !self.engine.has_loaded_audio() {
            return;
        }
        if !self.is_current() {
            return;
        }

        log::info!(
            "[QConnect] Stopping local playback because the active peer is playing (track_id={})",
            playback_state.track_id
        );
        if let Err(err) = self.engine.stop() {
            log::warn!("[QConnect] Failed to stop local playback after renderer handoff: {err}");
        }
    }

    /// Refresh the cached projection for the active renderer and, when a peer owns
    /// playback, align the local queue cursor to the peer's current track (so a
    /// later takeover lands on the right track). Mirrors the Tauri helper.
    async fn sync_active_renderer_projection(&self, renderer_id: i32) {
        if !self.is_current() {
            return;
        }
        let (queue_state, renderer_state, session_loop_mode, should_align_engine) = {
            let state = self.sync_state.lock().await;
            if !self.is_current() {
                return;
            }
            let Some(active_renderer_id) = state.session.active_renderer_id else {
                return;
            };
            if active_renderer_id != renderer_id {
                return;
            }

            (
                state.last_remote_queue_state.clone(),
                state
                    .session_renderer_states
                    .get(&active_renderer_id)
                    .cloned(),
                state.session_loop_mode,
                state.session.local_renderer_id != Some(active_renderer_id),
            )
        };

        let (Some(queue_state), Some(renderer_state)) = (queue_state, renderer_state) else {
            return;
        };

        let renderer_snapshot =
            build_session_renderer_snapshot(&queue_state, Some(&renderer_state), session_loop_mode);
        if !self.is_current() {
            return;
        }
        {
            let mut state = self.sync_state.lock().await;
            if !self.is_current() {
                return;
            }
            cache_renderer_snapshot(&mut state, &renderer_snapshot);
        }

        if !should_align_engine {
            return;
        }

        let Some(current_track) = renderer_snapshot.current_track.as_ref() else {
            return;
        };
        if !self.is_current() {
            return;
        }

        if let Err(err) =
            qconnect_app::renderer::align_queue_cursor(&self.engine, current_track.track_id).await
        {
            if !self.is_current() {
                return;
            }
            log::warn!("[QConnect] Failed to sync peer renderer cursor into engine: {err}");
        }
    }
}

#[async_trait]
impl QconnectEventSink for DaemonEventSink {
    fn allows_remote_volume(&self) -> bool {
        self.is_current() && self.volume_mode().applies_remote_volume()
    }

    fn playback_event(&self) -> Option<qbz_player::player::PlaybackEvent> {
        self.is_current().then(|| self.engine.playback_event())
    }

    async fn execute_renderer_command(
        &self, command: &RendererCommand, state: &qconnect_core::QConnectRendererState,
    ) -> Result<(), String> {
        if !self.is_current() {
            return Err("renderer authority retired".into());
        }
        if matches!(command, RendererCommand::SetVolume { .. } | RendererCommand::MuteVolume { .. })
            && !self.allows_remote_volume()
        {
            return Err("local output volume is locked".into());
        }
        if remote_renderer_commands_are_fenced(&*self.sync_state.lock().await) {
            return Err("renderer command fenced by local authority".into());
        }
        qconnect_app::renderer::apply_renderer_command(
            &self.engine, &self.sync_state, command, state,
        ).await?;
        if !self.is_current() {
            return Err("renderer authority retired during execution".into());
        }
        if matches!(command, RendererCommand::SetActive { active: true }) && self.engine.has_loaded_audio() {
            self.report_active_renderer_ready().await;
        }
        if !self.is_current() { return Err("renderer authority retired".into()); }
        Ok(())
    }

    async fn on_event(&self, event: QconnectAppEvent) {
        if !self.is_current() {
            return;
        }
        match &event {
            QconnectAppEvent::SessionManagementEvent {
                message_type,
                payload,
            } => {
                // Payloads can contain session identifiers and controller
                // topology. Log only the protocol message type.
                log::info!("[QConnect] Session management: {message_type}");
                self.apply_session_management_event(message_type, payload)
                    .await;
            }
            QconnectAppEvent::RendererUpdated(renderer_state) => {
                log::info!(
                    "[QConnect] Renderer updated: playing_state={:?} volume={:?} position={:?}",
                    renderer_state.playing_state,
                    renderer_state.volume,
                    renderer_state.current_position_ms,
                );
                let mut sync_state = self.sync_state.lock().await;
                if !self.is_current() {
                    return;
                }
                cache_renderer_snapshot(&mut sync_state, renderer_state);
            }
            QconnectAppEvent::QueueUpdated(queue_state) => {
                log::debug!(
                    "[QConnect] QueueUpdated: items={} shuffle_mode={} version={}.{}",
                    queue_state.queue_items.len(),
                    queue_state.shuffle_mode,
                    queue_state.version.major,
                    queue_state.version.minor,
                );
                let (should_materialize, accepted_local_echo) = {
                    let mut sync_state = self.sync_state.lock().await;
                    if !self.is_current() {
                        return;
                    }
                    sync_state.last_remote_queue_state = Some(queue_state.clone());
                    let had_local_takeover = sync_state.pending_local_queue_takeover.is_some();
                    let should_materialize =
                        should_materialize_remote_queue(&mut sync_state, queue_state);
                    let accepted_local_echo = had_local_takeover
                        && sync_state.pending_local_queue_takeover.is_none()
                        && sync_state.local_playback_state_assertion_pending;
                    (should_materialize, accepted_local_echo)
                };
                if !should_materialize {
                    if accepted_local_echo {
                        log::info!(
                            "[QConnect] Local queue accepted by Connect; preserving live player cursor"
                        );
                    } else {
                        log::debug!(
                            "[QConnect] Ignoring stale remote queue while local-playing takeover settles"
                        );
                    }
                    return;
                }
                if !self.is_current() {
                    return;
                }
                if let Err(err) = qconnect_app::renderer::materialize_remote_queue(
                    &self.engine,
                    &self.sync_state,
                    queue_state,
                )
                .await
                {
                    if !self.is_current() {
                        return;
                    }
                    log::warn!("[QConnect] Failed to materialize remote queue: {err}");
                }
            }
            QconnectAppEvent::RendererCommandApplied { .. } => {}
            QconnectAppEvent::RendererUnreachable { renderer_id } => {
                // Slint copy surfaced a toast here — daemon logs it (§1.4).
                log::warn!("[QConnect] Renderer {renderer_id} unreachable");
            }
            QconnectAppEvent::RendererDisconnected { renderer_id } => {
                // Slint copy surfaced a toast here — daemon logs it (§1.4).
                log::warn!("[QConnect] Renderer {renderer_id} disconnected");
            }
            QconnectAppEvent::PlaybackError {
                queue_item_id,
                error_type,
                ..
            } => {
                // Slint copy surfaced a toast here — daemon logs it (§1.4).
                log::warn!(
                    "[QConnect] Playback error on queue_item {queue_item_id}: {error_type:?}"
                );
            }
            QconnectAppEvent::ResyncComplete => {
                log::info!("[QConnect] Post-reconnect resync complete");
            }
            QconnectAppEvent::LifecycleChanged { state } => {
                log::info!("[QConnect] Lifecycle -> {state:?}");
            }
            QconnectAppEvent::Diagnostic { channel, level, .. } => {
                log::debug!("[QConnect] diagnostic {channel} [{level}]");
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod volume_tests {
    use super::*;
    use super::super::engine::VolumeMode;
    use crate::adapter::DaemonAdapter;
    use qbz_app::shell::AppRuntime;
    use qbz_audio::settings::AudioSettings;
    use qbz_models::Quality;
    use qconnect_protocol::{RendererCommandType, RendererServerCommand};
    use qconnect_transport_ws::{InMemoryWsTransport, TransportEvent, WsTransportConfig};
    use serde_json::json;

    // Exercise the actual daemon sink and engine through the shared app. No
    // authentication, loaded audio or physical output is needed. The child
    // process isolates the player's loudness cache from the user's profile.
    async fn exercise_volume_mode(mode: VolumeMode) {
        qbz_app::ensure_crypto_provider();
        let (adapter, _core_events) = DaemonAdapter::new();
        let runtime = Arc::new(AppRuntime::with_audio_settings(
            adapter, None, AudioSettings::default(), None,
        ));
        runtime.core().player().seed_volume_state(1.0);
        let authority = Arc::new(AuthorityCell::new());
        let stamp = authority.reserve(super::super::authority::AuthorityOrigin::Owner);
        assert!(authority.install(stamp));
        let engine = DaemonRendererEngine::new(
            Arc::clone(&runtime), mode,
            Arc::new(std::sync::Mutex::new(Quality::UltraHiRes)),
            Arc::clone(&authority), stamp,
        );
        let sync = Arc::new(Mutex::new(QconnectRemoteSyncState::default()));
        let sink = Arc::new(DaemonEventSink::new(
            engine, Arc::clone(&sync), Arc::clone(&authority), stamp,
            DaemonLanProjectionSlot::default(),
        ));
        assert_eq!(sink.allows_remote_volume(), mode == VolumeMode::Software);
        let transport = Arc::new(InMemoryWsTransport::new());
        let app = QconnectApp::new(Arc::clone(&transport), Arc::clone(&sink), sync);
        let _events = app.subscribe_transport_events();
        app.connect(WsTransportConfig::default()).await.unwrap();
        {
            let state = app.state_handle();
            let mut state = state.lock().await;
            state.renderer.volume = Some(100);
            state.renderer.muted = Some(false);
        }
        let initial = app.renderer_state_snapshot().await;
        for (command_type, payload, expected_volume, expected_muted, player_volume) in [
            (RendererCommandType::SrvrRndrSetVolume, json!({"volume": 40}), 40, false, 0.4),
            (RendererCommandType::SrvrRndrSetVolume, json!({"volume_delta": -5}), 35, false, 0.35),
            (RendererCommandType::SrvrRndrSetVolume, json!({"volume_delta": 5}), 40, false, 0.4),
            (RendererCommandType::SrvrRndrMuteVolume, json!({"value": true}), 40, true, 0.0),
            (RendererCommandType::SrvrRndrMuteVolume, json!({"value": false}), 40, false, 0.4),
        ] {
            let sent_before = transport.sent_messages().await.len();
            app.handle_transport_event(TransportEvent::InboundRendererServerCommand(
                RendererServerCommand { command_type, payload },
            )).await.unwrap();
            let state = app.renderer_state_snapshot().await;
            let messages = transport.sent_messages().await;
            if mode == VolumeMode::Locked {
                assert_eq!(state, initial);
                assert_eq!(messages.len(), sent_before);
                assert_eq!(runtime.core().get_playback_state().volume, 1.0);
            } else {
                assert_eq!(state.volume, Some(expected_volume));
                assert_eq!(state.muted, Some(expected_muted));
                // The player consumes SetVolume on its own command thread.
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    while (runtime.core().get_playback_state().volume - player_volume).abs() >= 0.001 {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                }).await.expect("player applies the accepted volume command");
                assert_eq!(messages.len(), sent_before + 1);
                let report = messages.last().unwrap();
                if command_type == RendererCommandType::SrvrRndrMuteVolume {
                    assert_eq!(report.message_type, "MESSAGE_TYPE_RNDR_SRVR_VOLUME_MUTED");
                    assert_eq!(report.payload["value"], expected_muted);
                } else {
                    assert_eq!(report.message_type, "MESSAGE_TYPE_RNDR_SRVR_VOLUME_CHANGED");
                    assert_eq!(report.payload["volume"], expected_volume);
                }
            }
        }
        // Retired engines must never advertise permission through the sink.
        authority.clear();
        assert!(!sink.allows_remote_volume());
    }

    #[tokio::test]
    async fn locked_daemon_ignores_volume_and_mute_without_false_reports() {
        if run_isolated_child("locked_daemon_ignores_volume_and_mute_without_false_reports") {
            return;
        }
        exercise_volume_mode(VolumeMode::Locked).await;
    }

    #[tokio::test]
    async fn software_daemon_applies_volume_and_mute_and_reports_real_changes() {
        if run_isolated_child("software_daemon_applies_volume_and_mute_and_reports_real_changes") {
            return;
        }
        exercise_volume_mode(VolumeMode::Software).await;
    }

    fn run_isolated_child(test: &str) -> bool {
        if std::env::var("QBZD_VOLUME_TEST_CHILD").as_deref() == Ok(test) {
            return false;
        }
        let root = tempfile::tempdir().unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &format!("qconnect::sink::volume_tests::{test}"), "--nocapture"])
            .env("QBZD_VOLUME_TEST_CHILD", test)
            .env("XDG_DATA_HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path())
            .env("XDG_CACHE_HOME", root.path())
            .env("APPDATA", root.path())
            .env("LOCALAPPDATA", root.path())
            .status().unwrap();
        assert!(result.success(), "isolated volume test failed: {test}");
        true
    }
}
