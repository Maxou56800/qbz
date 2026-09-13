//! Qt `QconnectEventSink` (block B3/B4 of the 2026-08-01 QConnect Qt-port
//! contract) — the inbound event dispatch.
//!
//! Behavior-1:1 port of the Slint `qbz/src/qconnect_event_sink.rs` (which itself
//! mirrors the Tauri `src-tauri/src/qconnect/event_sink.rs` arm-for-arm).
//! Receives `QconnectAppEvent`s from the qconnect-app crate and dispatches them
//! into the `QtRendererEngine` (the renderer seam), the shared
//! `QconnectRemoteSyncState` accumulator, and the Qt UI.
//!
//! UI surfacing per the contract: every Slint UI push becomes either a real
//! publish wired NOW — toasts via `crate::toast_qt` (msgids per contract §10),
//! is-remote/cast-target via `crate::now_playing::set_remote` (the reserved
//! seam, gated on `transport_connected` per the stale-badge fix), remote
//! volume-locked via `crate::now_playing::set_remote_volume_locked`, the queue
//! UI refresh via `crate::queue_qt::publish(runtime)`, now-playing meta /
//! shuffle-repeat via the same refresh functions `playback_qt.rs` uses — or
//! the `qconnect_qt::publish` layer (devices / active-renderer-id / dev
//! modal), which routes through the B4 `QbzQConnect` bridge.
//!
//! NOT ported forward, replicating the reference's own absences (§9 D5/D6):
//! controller-mode auto-skip on PlaybackError (the reference TODO) and the
//! lifecycle badge wiring. The module intentionally has no blanket dead-code
//! suppression: an orphaned event arm must warn.

use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use qconnect_app::{
    build_session_renderer_snapshot, cache_renderer_snapshot, is_peer_renderer_active,
    local_playback_should_yield_to_active_peer, remote_renderer_commands_are_fenced,
    renderer_allows_remote_volume, should_materialize_remote_queue, AuthorityCell, AuthorityStamp,
    QconnectApp, QconnectAppEvent, QconnectEventSink, QconnectRemoteSyncState,
    QconnectRendererEngine, RendererCommand,
};
use qconnect_transport_ws::NativeWsTransport;
use serde_json::Value;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::qconnect_engine_qt::QtRendererEngine;
use crate::qconnect_lan_qt::QtLanProjectionSlot;
use crate::qconnect_transport_qt::resolve_local_identity;

/// Concrete `QconnectApp` type used by the Qt adapter.
pub type QtQconnectApp = QconnectApp<NativeWsTransport, QtQconnectEventSink>;

const CTRL_SESSION_STATE_MESSAGE_TYPE: &str = "MESSAGE_TYPE_SRVR_CTRL_SESSION_STATE";

fn carries_lan_session_projection(message_type: &str) -> bool {
    message_type == CTRL_SESSION_STATE_MESSAGE_TYPE
}

pub struct QtQconnectEventSink {
    /// Renderer seam — forwards the `qconnect_app::renderer` orchestration onto
    /// `runtime.core()` + the protected player.
    engine: QtRendererEngine,
    /// Shared runtime — used to refresh the Qt now-playing card + queue
    /// panel from the (remotely-mutated) core state after inbound events.
    runtime: Arc<AppRuntime<LoggingAdapter>>,
    /// THE shared remote-sync accumulator (one Mutex, shared with `QconnectApp`).
    sync_state: Arc<Mutex<QconnectRemoteSyncState>>,
    /// Exact credential/runtime authority represented by this sink. Retired
    /// runtimes may still deliver late events, so every asynchronous boundary
    /// is followed by a stamp check before publishing or reporting.
    authority: Arc<AuthorityCell>,
    stamp: AuthorityStamp,
    /// Public LAN session projection. Updates are serialized with authority
    /// installs so late SESSION_STATE events cannot overwrite a replacement.
    projection: QtLanProjectionSlot,
    /// Late-bound weak handle to the owning app, wired via `set_app` after the
    /// app is built FROM this sink. Used to emit renderer reports (e.g.
    /// is_active=true after SetActive(true)) and to drive the session-apply +
    /// freeze/watchdog without an ownership cycle.
    app: Arc<OnceLock<Weak<QtQconnectApp>>>,
    /// FIX #13: previous "a peer is the active renderer" state, tracked across
    /// `apply_session_management_event` calls. On a false->true transition (QBZ
    /// becomes a CONTROLLER) we fire one `ask_for_active_renderer_state` to fetch
    /// the peer's full state (incl. `current_queue_item_id`), so the bar/queue
    /// resolve the peer's CURRENT track immediately instead of staying stale
    /// until the peer changes track. Edge-detected to avoid spamming on every
    /// periodic state-update frame.
    last_peer_active: std::sync::atomic::AtomicBool,
}

impl QtQconnectEventSink {
    pub fn new(
        engine: QtRendererEngine,
        runtime: Arc<AppRuntime<LoggingAdapter>>,
        sync_state: Arc<Mutex<QconnectRemoteSyncState>>,
        authority: Arc<AuthorityCell>,
        stamp: AuthorityStamp,
        projection: QtLanProjectionSlot,
    ) -> Self {
        Self {
            engine,
            runtime,
            sync_state,
            authority,
            stamp,
            projection,
            app: Arc::new(OnceLock::new()),
            last_peer_active: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn is_current(&self) -> bool {
        self.authority.is_current(self.stamp)
    }

    /// Rebuild the DEV-modal status block (session topology / renderer roles /
    /// queue) from the live sync state + app snapshot, and push it to the modal.
    async fn refresh_dev_status(&self) {
        if !self.is_current() {
            return;
        }
        let Some(app) = self.app.get().and_then(Weak::upgrade) else {
            return;
        };
        let queue = app.queue_state_snapshot().await;
        if !self.is_current() {
            return;
        }
        let status = {
            let st = self.sync_state.lock().await;
            if !self.is_current() {
                return;
            }
            let session = &st.session;
            let role = match (session.active_renderer_id, session.local_renderer_id) {
                (Some(a), Some(l)) if a == l => "renderer (this device active)",
                (Some(_), Some(_)) => "controller (peer active)",
                (Some(_), None) => "joined (local id pending)",
                _ => "no active renderer",
            };
            let renderers = if session.renderers.is_empty() {
                "  (none)".to_string()
            } else {
                session
                    .renderers
                    .iter()
                    .map(|r| {
                        let local = if Some(r.renderer_id) == session.local_renderer_id {
                            " LOCAL"
                        } else {
                            ""
                        };
                        let active = if Some(r.renderer_id) == session.active_renderer_id {
                            " ACTIVE"
                        } else {
                            ""
                        };
                        format!(
                            "  #{} {}{}{}",
                            r.renderer_id,
                            r.friendly_name.clone().unwrap_or_default(),
                            local,
                            active
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let session_presence = if session.session_uuid.is_some() {
                "present"
            } else {
                "absent"
            };
            format!(
                "Role: {role}\nsession: {session_presence}\nactive_renderer_id: {:?}   local_renderer_id: {:?}\nqueue: v{}.{}  items={}  autoplay={}\nrenderers:\n{}",
                session.active_renderer_id,
                session.local_renderer_id,
                queue.version.major,
                queue.version.minor,
                queue.queue_items.len(),
                queue.autoplay_items.len(),
                renderers,
            )
        };
        if !self.is_current() {
            return;
        }
        crate::qconnect_qt::dev_set_status(status);
    }

    /// Rebuild the QConnect device-picker model from the live session topology
    /// and publish it (devices + active-renderer-id). Mirrors the Tauri
    /// renderer-list source. Maps `session.renderers` -> rows, marking the
    /// local device (`is-local`, rendered as "Play here") and the active one.
    async fn refresh_device_list(&self) {
        if !self.is_current() {
            return;
        }
        let (devices, active_id) = {
            let st = self.sync_state.lock().await;
            if !self.is_current() {
                return;
            }
            let session = &st.session;
            let devices: Vec<crate::qconnect_qt::publish::QconnectDeviceRow> = session
                .renderers
                .iter()
                .map(|r| crate::qconnect_qt::publish::QconnectDeviceRow {
                    renderer_id: r.renderer_id,
                    name: r
                        .friendly_name
                        .clone()
                        .unwrap_or_else(|| "Unknown device".to_string()),
                    is_local: Some(r.renderer_id) == session.local_renderer_id,
                    is_active: Some(r.renderer_id) == session.active_renderer_id,
                    icon: device_icon_key(r.device_type, r.friendly_name.as_deref().unwrap_or("")),
                    device_uuid: r.device_uuid.clone(),
                })
                .collect();
            (devices, session.active_renderer_id.unwrap_or(-1))
        };

        if !self.is_current() {
            return;
        }
        crate::qconnect_qt::publish::devices(devices);
        if !self.is_current() {
            return;
        }
        crate::qconnect_qt::publish::active_renderer_id(active_id);
    }

    /// Push the cast-aware now-playing state (is-remote / cast-target /
    /// volume-locked) from the live session topology. Replaces the old
    /// `TODO(slint-qconnect-ui)`. `is-remote` is true when a PEER renderer owns
    /// playback; `cast-target` is its friendly name; `volume-locked` is true
    /// when that renderer disallows remote volume. This is THE writer of
    /// `np_is_remote` / `np_cast_target` (the reserved `now_playing::set_remote`
    /// seam, named by `cast_qt.rs` as the qconnect event sink's job) and the
    /// primary writer of `np_remote_volume_locked` (contract §11.3; the second
    /// write site is the facade's disconnect tail).
    async fn refresh_now_playing_remote_state(&self) {
        if !self.is_current() {
            return;
        }
        // Badge gate: if the transport is down (terminal teardown OR a transient
        // reconnect blip), the renderer/controller badge must read NOT remote. The
        // in-memory session can still name a peer as active_renderer_id long after
        // the session ended (freeze sets playing_state=UNKNOWN but leaves
        // active_renderer_id, and disconnect() only runs on the user toggle, not on
        // transport-drop / reconnect-exhausted). Mirrors the Tauri
        // fetchQconnectRuntimeState early-return on !transport_connected. On
        // reconnect, TransportConnected re-runs this refresh and the repopulated
        // session restores the badge.
        let transport_connected = match self.app.get().and_then(Weak::upgrade) {
            Some(app) => {
                let state_handle = app.state_handle();
                let state = state_handle.lock().await;
                if !self.is_current() {
                    return;
                }
                state.transport_connected
            }
            None => false,
        };
        let (is_remote, cast_target, volume_locked) = {
            let st = self.sync_state.lock().await;
            if !self.is_current() {
                return;
            }
            let session = &st.session;
            let is_remote = transport_connected && is_peer_renderer_active(session);
            let active = session.active_renderer_id;
            let active_info = active.and_then(|active_id| {
                session
                    .renderers
                    .iter()
                    .find(|r| r.renderer_id == active_id)
            });
            let cast_target = if is_remote {
                active_info
                    .and_then(|r| r.friendly_name.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let volume_locked = is_remote
                && active_info
                    .map(|r| !renderer_allows_remote_volume(r))
                    .unwrap_or(false);
            (is_remote, cast_target, volume_locked)
        };

        if !self.is_current() {
            return;
        }
        crate::now_playing::set_remote(is_remote, &cast_target);
        if !self.is_current() {
            return;
        }
        crate::now_playing::set_remote_volume_locked(volume_locked);
    }

    /// Refresh the Qt now-playing card + queue panel from the current core
    /// state. The inbound renderer orchestration (materialize / apply) mutates
    /// the core player+queue but does NOT touch the UI; without this the card +
    /// queue stay on whatever was loaded at connect time while the audio follows
    /// the remote controller. Reads core `current_track()` so it is authoritative
    /// regardless of the order QueueUpdated / SetState arrive in.
    async fn refresh_local_ui(&self) {
        if !self.is_current() {
            return;
        }
        let current = self.runtime.core().current_track().await;
        if !self.is_current() {
            return;
        }
        // Queue edits and repeated SetState do not select a new track. Replacing
        // the metadata model here would reset its elapsed time and artwork.
        let displayed = crate::now_playing::art_seed();
        if current.as_ref().map(|track| track.id) != Some(displayed.track_id) {
            crate::playback_qt::refresh_now_playing(&self.runtime).await;
        }
        if !self.is_current() {
            return;
        }
        // The reference also refreshes the queue sidebar here
        // (`refresh_sidebar(true)`); the Qt queue panel document is the same
        // data surface, published through the existing `queue_qt::publish` path.
        crate::queue_qt::publish(&self.runtime).await;
        if !self.is_current() {
            return;
        }
        self.refresh_transport_modes().await;
    }

    /// Reflect the cloud-authoritative shuffle/repeat on the bar buttons from the
    /// materialized CORE queue. The inbound SetShuffleMode / QueueUpdated update
    /// the CORE queue (the cloud's order + flags), but never the UI button — this
    /// pushes them. Lightweight (no card/art refresh) so it is safe to call on a
    /// standalone shuffle/loop command without resetting the now-playing card.
    /// Matches Tauri, whose button reads `queue.shuffle` (not a per-renderer
    /// field the cloud never populates for a peer).
    async fn refresh_transport_modes(&self) {
        if !self.is_current() {
            return;
        }
        let qs = self.runtime.core().get_queue_state().await;
        if !self.is_current() {
            return;
        }
        let shuffle_on = qs.shuffle;
        let repeat_mode = match qs.repeat {
            qbz_models::RepeatMode::Off => 0,
            qbz_models::RepeatMode::All => 1,
            qbz_models::RepeatMode::One => 2,
        };
        crate::now_playing::set_shuffle(shuffle_on);
        if !self.is_current() {
            return;
        }
        crate::now_playing::set_repeat_mode(repeat_mode);
    }

    /// Wire the owning app after construction. Idempotent (OnceLock).
    pub fn set_app(&self, app: &Arc<QtQconnectApp>) {
        let _ = self.app.set(Arc::downgrade(app));
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
        let result = app.send_renderer_report_command(report).await;
        if !self.is_current() {
            return;
        }
        if let Err(err) = result {
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

        if carries_lan_session_projection(message_type) {
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
            let result =
                qconnect_app::renderer::apply_remote_loop_mode(&self.engine, loop_mode).await;
            if !self.is_current() {
                return;
            }
            if let Err(err) = result {
                log::warn!("[QConnect] Failed to apply remote loop mode: {err}");
            }
        }

        if outcome.sync_local_playback {
            self.sync_local_playback_for_renderer_ownership().await;
            if !self.is_current() {
                return;
            }
        }

        if let Some(renderer_id) = outcome.remote_projection_renderer_id {
            self.sync_active_renderer_projection(renderer_id).await;
            if !self.is_current() {
                return;
            }
        }

        if let Some(renderer_id) = outcome.disconnected_renderer_id {
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

        // Request the newly active peer's state once on controller entry, so
        // projection does not have to wait for its next periodic update. An
        // omitted scalar current_queue_item_id INSIDE player_state means 0
        // (proto3), not "position-only"; the protocol decoder restores it.
        let peer_active_now = {
            let state = self.sync_state.lock().await;
            if !self.is_current() {
                return;
            }
            is_peer_renderer_active(&state.session)
        };
        if !self.is_current() {
            return;
        }
        let was_peer_active = self
            .last_peer_active
            .swap(peer_active_now, std::sync::atomic::Ordering::Relaxed);
        let conflict_pending = {
            let state = self.sync_state.lock().await;
            state.local_playback_conflict_pending
        };
        if peer_active_now && !was_peer_active && !conflict_pending {
            let result = app.ask_for_active_renderer_state().await;
            if !self.is_current() {
                return;
            }
            if let Err(err) = result {
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
            local_playback_should_yield_to_active_peer(&state)
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
    /// playback, align the local queue cursor to the peer's current track (so the
    /// controller view + a later takeover land on the right track). Mirrors the
    /// Tauri helper.
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

        let result =
            qconnect_app::renderer::align_queue_cursor(&self.engine, current_track.track_id).await;
        if !self.is_current() {
            return;
        }
        if let Err(err) = result {
            log::warn!("[QConnect] Failed to sync peer renderer cursor into engine: {err}");
        }
    }
}

#[async_trait]
impl QconnectEventSink for QtQconnectEventSink {
    fn playback_event(&self) -> Option<qbz_player::player::PlaybackEvent> {
        self.is_current().then(|| self.engine.playback_event())
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
                // The server echoes renderer position/state every two seconds.
                // Keep topology/session changes visible at info, but do not turn
                // ordinary playback into an unbounded terminal transcript. The
                // payload is intentionally never logged: it can carry session and
                // delegated-credential material.
                if message_type == "MESSAGE_TYPE_SRVR_CTRL_RENDERER_STATE_UPDATED" {
                    log::debug!("[QConnect] Session management: {message_type}");
                } else if message_type == "MESSAGE_TYPE_SRVR_CTRL_LOOP_MODE_SET" {
                    // This allowlisted scalar makes an actual mode transition
                    // break the consecutive-log run; never print the payload.
                    let loop_mode = payload.get("loop_mode").and_then(Value::as_i64);
                    log::info!("[QConnect] Session management: {message_type} loop_mode={loop_mode:?}");
                } else {
                    log::info!("[QConnect] Session management: {message_type}");
                }
                self.apply_session_management_event(message_type, payload)
                    .await;
                if !self.is_current() {
                    return;
                }
            }
            QconnectAppEvent::RendererUpdated(renderer_state) => {
                log::debug!(
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
                let result = qconnect_app::renderer::materialize_remote_queue(
                    &self.engine,
                    &self.sync_state,
                    queue_state,
                )
                .await;
                if !self.is_current() {
                    return;
                }
                let materialized = match result {
                    Ok(materialized) => materialized,
                    Err(err) => {
                        log::warn!("[QConnect] Failed to materialize remote queue: {err}");
                        false
                    }
                };
                if !materialized {
                    return;
                }
                // Reflect the remote queue change in the QBZ UI (queue panel +
                // now-playing card). materialize already set the core queue +
                // cursor; this just pushes it to Qt.
                self.refresh_local_ui().await;
                if !self.is_current() {
                    return;
                }
            }
            QconnectAppEvent::RendererCommandApplied { command, state } => {
                let fenced = {
                    let sync_state = self.sync_state.lock().await;
                    remote_renderer_commands_are_fenced(&sync_state)
                };
                if fenced {
                    log::info!(
                        "[QConnect] Ignoring stale renderer command while local queue authority settles"
                    );
                    return;
                }
                // SetState is the routine playback/position command and may be
                // republished. Lifecycle commands remain visible at info.
                if matches!(command, RendererCommand::SetState { .. }) {
                    log::debug!(
                        "[QConnect] Renderer command applied: {}",
                        renderer_command_label(command)
                    );
                } else {
                    log::info!(
                        "[QConnect] Renderer command applied: {}",
                        renderer_command_label(command)
                    );
                }
                let became_active = matches!(command, RendererCommand::SetActive { active: true });
                let result = qconnect_app::renderer::apply_renderer_command(
                    &self.engine,
                    &self.sync_state,
                    command,
                    state,
                )
                .await;
                if !self.is_current() {
                    return;
                }
                let applied = if let Err(err) = result {
                    log::warn!("[QConnect] Failed to apply renderer command: {err}");
                    false
                } else if became_active {
                    self.report_active_renderer_ready().await;
                    if !self.is_current() {
                        return;
                    }
                    true
                } else {
                    true
                };
                if applied {
                    let (volume, muted) = local_volume_ui_projection(command, state);
                    if let Some(volume) = volume {
                        crate::now_playing::set_volume(volume);
                    }
                    if let Some(muted) = muted {
                        crate::now_playing::set_muted(muted);
                    }
                }
                // A SetState changes the current track / play-state — reflect it
                // in the QBZ now-playing card + queue cursor highlight. A
                // standalone SetShuffleMode / SetLoopMode does NOT move the track,
                // so only refresh the lightweight shuffle/repeat button state (a
                // full refresh would reset the now-playing card position/art).
                if matches!(command, RendererCommand::SetState { .. }) {
                    self.refresh_local_ui().await;
                    if !self.is_current() {
                        return;
                    }
                } else if matches!(
                    command,
                    RendererCommand::SetShuffleMode { .. } | RendererCommand::SetLoopMode { .. }
                ) {
                    self.refresh_transport_modes().await;
                    if !self.is_current() {
                        return;
                    }
                }
            }
            QconnectAppEvent::RendererUnreachable { renderer_id } => {
                log::warn!("[QConnect] Renderer {renderer_id} unreachable");
                crate::toast_qt::error(qbz_i18n::t("Qobuz Connect renderer unreachable"));
            }
            QconnectAppEvent::RendererDisconnected { renderer_id } => {
                log::warn!("[QConnect] Renderer {renderer_id} disconnected");
                crate::toast_qt::error(qbz_i18n::t("Qobuz Connect renderer disconnected"));
            }
            QconnectAppEvent::PlaybackError {
                queue_item_id,
                error_type,
                ..
            } => {
                // TODO(qt-qconnect-ui): when QBZ is the controller, auto-skip the
                // current item. For now surface the failure. (Reference
                // TODO(slint-qconnect-ui); kept unwired per §9 D5.)
                log::warn!(
                    "[QConnect] Playback error on queue_item {queue_item_id}: {error_type:?}"
                );
                crate::toast_qt::error(qbz_i18n::t("Track unavailable on Qobuz Connect"));
            }
            QconnectAppEvent::ResyncComplete => {
                log::info!("[QConnect] Post-reconnect resync complete");
            }
            QconnectAppEvent::LifecycleChanged { state } => {
                // TODO(qt-qconnect-ui): drive the connect badge state.
                // (Reference TODO(slint-qconnect-ui); kept unwired per §9 D6.)
                log::info!("[QConnect] Lifecycle -> {state:?} (UI badge TODO)");
            }
            QconnectAppEvent::Diagnostic { channel, level, .. } => {
                log::debug!("[QConnect] diagnostic {channel} [{level}]");
            }
            _ => {}
        }

        if !self.is_current() {
            return;
        }
        // DEV diagnostics: log every event (with a relative timestamp) + refresh
        // the live status block, so the QconnectDevModal reflects QC state at
        // runtime without a rebuild.
        crate::qconnect_qt::dev_push_event(dev_event_line(&event));
        self.refresh_dev_status().await;
        if !self.is_current() {
            return;
        }

        // Controller-mode UI: rebuild the device picker + push the cast-aware
        // now-playing state (is-remote / cast-target / volume-locked) from the
        // live session topology after every event.
        self.refresh_device_list().await;
        if !self.is_current() {
            return;
        }
        self.refresh_now_playing_remote_state().await;
    }
}

/// Map a renderer's `device_type` (+ a name heuristic for web players) to a
/// device-icon key, mirroring the Tauri `QconnectBadge.resolveDeviceType`:
/// 6 = mobile, 5 = computer (or "web" when the name says web player/browser),
/// anything else (3/4/…) = speaker/receiver.
fn device_icon_key(device_type: Option<i32>, friendly_name: &str) -> &'static str {
    match device_type.unwrap_or(5) {
        6 => "mobile",
        5 => {
            let name = friendly_name.to_ascii_lowercase();
            if name.contains("web player") || name.contains("browser") {
                "web"
            } else {
                "computer"
            }
        }
        _ => "speaker",
    }
}

fn renderer_command_label(command: &RendererCommand) -> &'static str {
    match command {
        RendererCommand::SetState { .. } => "set_state",
        RendererCommand::SetVolume { .. } => "set_volume",
        RendererCommand::SetActive { .. } => "set_active",
        RendererCommand::SetMaxAudioQuality { .. } => "set_max_audio_quality",
        RendererCommand::SetLoopMode { .. } => "set_loop_mode",
        RendererCommand::SetShuffleMode { .. } => "set_shuffle_mode",
        RendererCommand::MuteVolume { .. } => "mute_volume",
    }
}

/// Project a controller-originated volume command onto the local QBZ slider.
/// Applying the command to the audio engine is not enough: ordinary local
/// software volume is deliberately excluded from the 1 Hz hardware-knob poll,
/// so without this command-edge projection the sound changes while QML remains
/// at its previous value.
fn local_volume_ui_projection(
    command: &RendererCommand,
    state: &qconnect_app::QConnectRendererState,
) -> (Option<f32>, Option<bool>) {
    match command {
        RendererCommand::SetVolume { volume, .. } => {
            let volume = state
                .volume
                .or(*volume)
                .map(qconnect_app::renderer::normalize_volume_to_fraction);
            (volume, volume.map(|value| value <= 0.0))
        }
        RendererCommand::MuteVolume { value } if *value => (Some(0.0), Some(true)),
        RendererCommand::MuteVolume { .. } => (
            state
                .volume
                .map(qconnect_app::renderer::normalize_volume_to_fraction),
            Some(false),
        ),
        _ => (None, None),
    }
}

/// Format a QConnect event into a one-line DEV-log entry. Big payloads
/// (QueueUpdated / SessionManagement) are summarized; the rest use Debug.
fn dev_event_line(event: &QconnectAppEvent) -> String {
    match event {
        QconnectAppEvent::SessionManagementEvent { message_type, .. } => {
            format!("SESSION {message_type}")
        }
        QconnectAppEvent::QueueUpdated(q) => format!(
            "QueueUpdated v{}.{} items={} shuffle={}",
            q.version.major,
            q.version.minor,
            q.queue_items.len(),
            q.shuffle_mode
        ),
        QconnectAppEvent::RendererUpdated(r) => format!(
            "RendererUpdated playing={:?} pos={:?}ms vol={:?}",
            r.playing_state, r.current_position_ms, r.volume
        ),
        QconnectAppEvent::RendererCommandApplied { command, .. } => {
            format!("Cmd {}", renderer_command_label(command))
        }
        QconnectAppEvent::RendererUnreachable { renderer_id } => {
            format!("RendererUnreachable #{renderer_id}")
        }
        QconnectAppEvent::RendererDisconnected { renderer_id } => {
            format!("RendererDisconnected #{renderer_id}")
        }
        QconnectAppEvent::PlaybackError {
            queue_item_id,
            error_type,
            ..
        } => format!("PlaybackError qid={queue_item_id} {error_type:?}"),
        QconnectAppEvent::LifecycleChanged { state } => format!("Lifecycle {state:?}"),
        QconnectAppEvent::ResyncComplete => "ResyncComplete".to_string(),
        QconnectAppEvent::TransportConnected => "TransportConnected".to_string(),
        QconnectAppEvent::TransportDisconnected => "TransportDisconnected".to_string(),
        QconnectAppEvent::Diagnostic { channel, level, .. } => format!("diag {channel} [{level}]"),
        QconnectAppEvent::PendingActionStarted { .. } => "PendingActionStarted".to_string(),
        QconnectAppEvent::PendingActionCompleted { .. } => "PendingActionCompleted".to_string(),
        QconnectAppEvent::PendingActionTimedOut { timeout_ms, .. } => {
            format!("PendingActionTimedOut after={timeout_ms}ms")
        }
        QconnectAppEvent::PendingActionCanceledByConcurrentRemoteEvent { .. } => {
            "PendingActionCanceledByConcurrentRemoteEvent".to_string()
        }
        QconnectAppEvent::QueueErrorIgnoredByConcurrency { .. } => {
            "QueueErrorIgnoredByConcurrency".to_string()
        }
        QconnectAppEvent::QueueResyncTriggered => "QueueResyncTriggered".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ctrl_session_state_events_carry_a_lan_session_projection() {
        assert!(carries_lan_session_projection(
            CTRL_SESSION_STATE_MESSAGE_TYPE
        ));
        assert!(!carries_lan_session_projection(
            "MESSAGE_TYPE_SRVR_CTRL_RENDERER_STATE"
        ));
        assert!(!carries_lan_session_projection(
            "MESSAGE_TYPE_SRVR_CTRL_QUEUE_STATE"
        ));
    }

    #[test]
    fn local_renderer_volume_command_projects_to_the_qbz_slider() {
        let command = RendererCommand::SetVolume {
            volume: Some(10),
            volume_delta: None,
        };
        let state = qconnect_app::QConnectRendererState {
            volume: Some(73),
            ..Default::default()
        };

        let (volume, muted) = local_volume_ui_projection(&command, &state);

        assert!((volume.unwrap() - 0.73).abs() < f32::EPSILON);
        assert_eq!(muted, Some(false));
    }

    #[test]
    fn local_renderer_mute_edges_project_both_slider_and_icon() {
        let state = qconnect_app::QConnectRendererState {
            volume: Some(42),
            ..Default::default()
        };
        assert_eq!(
            local_volume_ui_projection(&RendererCommand::MuteVolume { value: true }, &state),
            (Some(0.0), Some(true))
        );
        assert_eq!(
            local_volume_ui_projection(&RendererCommand::MuteVolume { value: false }, &state),
            (Some(0.42), Some(false))
        );
    }
}
