// TODO(converge: qconnect-glue) — copied from crates/qbz/src/qconnect_service.rs @ 5d50158e;
// do not fix bugs here without fixing the source, and vice versa.
//
//! Daemon session-loop host + controller bootstrap + deferred renderer-join.
//!
//! [`DaemonSessionLoopHost`] implements the frontend-agnostic
//! `qconnect_app::SessionLoopHost` so the shared `run_session_loop` drives
//! lifecycle, reconnect bootstrap/resync, deferred renderer-join, and
//! reconnect-exhausted teardown through this adapter — exactly as
//! `SlintSessionLoopHost` does.
//!
//! Two DELIBERATE daemon-copy behavior fixes vs. the desktop (OD2 — desktop call
//! sites untouched; owner may fold these back into the shared path later):
//!   (a) `deferred_renderer_join` reports the player's REAL volume instead of the
//!       hardcoded `json!({"volume": 100})` (qconnect_service.rs:2711-2715; §7.4).
//!   (b) `bootstrap_after_reconnect` RE-RESOLVES transport credentials via
//!       `resolve_transport_config` so a reconnect after JWT expiry recovers
//!       (IV3 — the WS transport reuses `last_config` and never re-resolves
//!       `/qws/createToken`; design-input/qconnect-headless.md §7.6.7).
//!
//! The desktop's D5 offline-mode parks are dropped: qbzd has no offline MODE.

use std::sync::{Arc, Mutex as StdMutex};

use qbz_app::shell::AppRuntime;
use qconnect_app::{
    QconnectLifecycleState, QconnectRemoteSyncState, QueueCommandType, RendererReport,
    RendererReportType, SessionLoopHost, JOIN_SESSION_REASON_RECONNECTION,
};
use serde_json::json;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::authority::{AuthorityCell, AuthorityStamp};
use super::engine::VolumeMode; // T10 (OD4): join-time volume report honors the mode
use super::lan::DaemonLanProjectionSlot;
use super::sink::{DaemonEventSink, DaemonQconnectApp};
use super::transport::{
    default_qconnect_device_info, default_qconnect_device_info_with_name, load_persisted_device_name,
    resolve_transport_config, QconnectJoinSessionRequest, AUDIO_QUALITY_HIRES_LEVEL2,
};
use super::{update_lifecycle_state_if_running, DaemonQconnectInner};
use crate::adapter::DaemonAdapter;
use crate::state::DaemonShared;

type Runtime = Arc<AppRuntime<DaemonAdapter>>;

/// Daemon-side implementation of the shared session-loop seams. Holds the handles
/// the loop reaches back into: the app (renderer join + state reads), the shared
/// sync accumulator, the service inner (lifecycle gating + teardown + config
/// re-latch), the sink (lifecycle emit), the runtime (track-duration + volume
/// reads), and the daemon shared state (status latching for `/api/status`).
pub struct DaemonSessionLoopHost {
    pub app: Arc<DaemonQconnectApp>,
    pub sync_state: Arc<Mutex<QconnectRemoteSyncState>>,
    pub inner: Arc<StdMutex<DaemonQconnectInner>>,
    pub authority: Arc<AuthorityCell>,
    pub stamp: AuthorityStamp,
    pub sink: Arc<DaemonEventSink>,
    pub runtime: Runtime,
    pub shared: Arc<std::sync::Mutex<DaemonShared>>,
    /// T10 (OD4): resolved volume policy — the deferred renderer join reports 100
    /// in `Locked` mode, the real player volume in `Software`.
    pub volume_mode: VolumeMode,
    pub(crate) projection: DaemonLanProjectionSlot,
}

#[async_trait::async_trait]
impl SessionLoopHost for DaemonSessionLoopHost {
    fn local_playback_is_playing(&self) -> bool {
        self.authority.is_current(self.stamp) && self.runtime.core().get_playback_state().is_playing
    }

    async fn publish_local_queue_for_takeover(&self) -> bool {
        super::publish::publish_local_queue_for_takeover(
            &self.inner,
            &self.runtime,
            &self.authority,
            self.stamp,
        )
        .await
    }

    async fn stop_local_playback_for_remote_queue(&self) {
        if !self.authority.is_current(self.stamp) {
            return;
        }
        if let Err(error) = self.runtime.core().stop() {
            log::warn!("[QConnect] Failed to stop daemon playback for remote queue: {error}");
        }
    }

    async fn update_lifecycle(&self, state: QconnectLifecycleState) {
        if !self.authority.is_current(self.stamp) {
            return;
        }
        update_lifecycle_state_if_running(
            &self.inner,
            &self.sink,
            &self.shared,
            &self.authority,
            self.stamp,
            state,
        )
        .await;
    }

    async fn bootstrap_after_reconnect(&self) {
        if !self.authority.is_current(self.stamp) {
            return;
        }
        // FIX (b) (IV3, daemon-copy only): the WS transport reuses `last_config`
        // on in-loop reconnects and never re-resolves `/qws/createToken`, so a
        // reconnect after the JWT expires keeps failing. Re-resolve the transport
        // credentials (fresh `/qws/createToken`) and latch the new config into the
        // runtime so a subsequent full reconnect uses valid credentials. The
        // desktop copy does NOT do this.
        match resolve_transport_config(&self.runtime).await {
            Ok(fresh) => {
                if !self.authority.is_current(self.stamp) {
                    return;
                }
                let latched = {
                    let mut guard = self
                        .inner
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if let Some(rt) = guard.runtime.as_mut() {
                        if rt.stamp == self.stamp {
                            rt.config = fresh;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };
                if !latched || !self.authority.is_current(self.stamp) {
                    return;
                }
                log::info!("[QConnect] reconnect: re-resolved transport credentials (fresh JWT)");
            }
            Err(err) => {
                if !self.authority.is_current(self.stamp) {
                    return;
                }
                log::warn!("[QConnect] reconnect: credential re-resolve failed: {err}");
            }
        }
        if !self.authority.is_current(self.stamp) {
            return;
        }
        // Announce the same persisted name the local identity matches against;
        // the default name would break the renderer self-match after reconnect.
        let device_name = load_persisted_device_name();
        log::info!("[QConnect] reconnect: announcing device name {device_name:?}");
        if let Err(err) =
            bootstrap_remote_presence(&self.app, device_name, &self.authority, self.stamp).await
        {
            if !self.authority.is_current(self.stamp) {
                return;
            }
            log::error!("[QConnect] Re-bootstrap after reconnect failed: {err}");
        }
    }

    async fn deferred_renderer_join(&self, session_uuid: String, reason: i32) {
        deferred_renderer_join(
            &self.app,
            &self.sync_state,
            &self.runtime,
            self.volume_mode, // T10 (OD4): 100 in Locked, real in Software
            &session_uuid,
            reason,
            &self.authority,
            self.stamp,
        )
        .await;
    }

    async fn on_reconnect_exhausted(
        &self,
        attempts: u32,
        last_reason: String,
        idle_retry_active: bool,
    ) -> bool {
        if !self.authority.is_current(self.stamp) {
            return true;
        }
        let (applied, retired) = {
            let mut guard = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.runtime.as_ref().map(|rt| rt.stamp) != Some(self.stamp) {
                (false, None)
            } else {
                guard.lifecycle_state = QconnectLifecycleState::Exhausted;
                guard.last_error = Some(format!(
                    "Reconnect attempts exhausted ({attempts}): {last_reason}"
                ));
                if !idle_retry_active {
                    // Legacy terminate path: drop the runtime so a fresh connect()
                    // succeeds. Dropping it detaches this task's own JoinHandle
                    // (fine — the loop breaks right after).
                    (true, guard.runtime.take())
                } else {
                    (true, None)
                }
            }
        };
        // Never drop a retired runtime while holding the synchronous inner lock.
        drop(retired);
        if !applied || !self.authority.is_current(self.stamp) {
            return true;
        }
        if let Ok(mut s) = self.shared.lock() {
            if !self.authority.is_current(self.stamp) {
                return true;
            }
            s.qconnect.state = "exhausted".to_string();
            s.qconnect.session_active = false;
            // 01 §9.3: reconnect-exhausted is a real network-class failure —
            // latch `network.online` false (the success side latches true in
            // `latch_lifecycle_into_shared` on the next `Connected` transition).
            s.set_network_online(false);
            s.emit_qconnect_session_changed();
        }
        log::warn!("[QConnect] Reconnect exhausted ({attempts}): {last_reason}");
        if !idle_retry_active {
            // Invalidate this sink/host after publishing the terminal status.
            // `clear_if_current` cannot clear a replacement installed in between.
            self.projection
                .clear_if_current(&self.authority, self.stamp);
        }
        // Daemon has no offline MODE, so the desktop's idle-retry offline park is
        // dropped: keep idling (the transport's internal 60s idle rearm keeps
        // trying) unless idle-retry is off, in which case break + drop.
        !idle_retry_active
    }

    async fn on_loop_error(&self, message: String) {
        if !self.authority.is_current(self.stamp) {
            return;
        }
        // Slint copy surfaced a toast here — daemon logs it.
        log::error!("[QConnect] session loop error: {message}");
    }
}

/// Controller-side bootstrap: JoinSession (works without a session_uuid) then ask
/// for the current queue state. The renderer-side join is deferred until the
/// server sends SESSION_STATE with a session_uuid (handled in the session loop).
/// Mirrors the Tauri `bootstrap_remote_presence`.
pub async fn bootstrap_remote_presence(
    app: &Arc<DaemonQconnectApp>,
    custom_device_name: Option<String>,
    authority: &AuthorityCell,
    stamp: AuthorityStamp,
) -> Result<(), String> {
    bootstrap_remote_presence_with_gate(app, custom_device_name, || authority.is_current(stamp))
        .await
}

/// Complete the owner bootstrap on an isolated, authenticated/subscribed QWS
/// runtime before its authority commit. The receiver is already subscribed, so
/// resulting session events remain buffered for the loop installed at commit.
pub(crate) async fn bootstrap_prepared_owner_presence(
    app: &Arc<DaemonQconnectApp>,
    custom_device_name: Option<String>,
) -> Result<(), String> {
    bootstrap_remote_presence_with_gate(app, custom_device_name, || true).await
}

async fn bootstrap_remote_presence_with_gate<F>(
    app: &Arc<DaemonQconnectApp>,
    custom_device_name: Option<String>,
    is_current: F,
) -> Result<(), String>
where
    F: Fn() -> bool,
{
    if !is_current() {
        return Ok(());
    }
    let device_info = default_qconnect_device_info_with_name(custom_device_name.as_deref());

    let join_payload = serde_json::to_value(QconnectJoinSessionRequest {
        session_uuid: None,
        device_info: Some(device_info),
    })
    .map_err(|err| format!("serialize join_session bootstrap payload: {err}"))?;

    let join_command = app
        .build_queue_command(QueueCommandType::CtrlSrvrJoinSession, join_payload)
        .await;
    if !is_current() {
        return Ok(());
    }
    let join_action_uuid = match app.send_queue_command(join_command).await {
        Ok(action_uuid) => action_uuid,
        Err(_) if !is_current() => return Ok(()),
        Err(err) => {
            return Err(format!(
                "send bootstrap ctrl_srvr_join_session failed: {err}"
            ))
        }
    };
    // JoinSession responds with session/renderer controller events not part of
    // queue reducer correlation. Drop the pending slot so queue ops aren't blocked.
    app.clear_pending_if_matches(&join_action_uuid).await;
    if !is_current() {
        return Ok(());
    }

    let ask_queue_command = app
        .build_queue_command(QueueCommandType::CtrlSrvrAskForQueueState, json!({}))
        .await;
    if !is_current() {
        return Ok(());
    }
    let ask_action_uuid = match app.send_queue_command(ask_queue_command).await {
        Ok(action_uuid) => action_uuid,
        Err(_) if !is_current() => return Ok(()),
        Err(err) => return Err(format!("send bootstrap ask_for_queue_state failed: {err}")),
    };
    app.clear_pending_if_matches(&ask_action_uuid).await;
    if !is_current() {
        return Ok(());
    }

    log::info!(
        "[QConnect] Bootstrap complete: controller joined, queue state requested. Renderer join deferred until session_uuid received."
    );
    Ok(())
}

/// Deferred renderer join: called from the session loop when SESSION_STATE with a
/// session_uuid arrives. Idempotent per uuid (P1-8). Mirrors the Tauri
/// `deferred_renderer_join`, reading the current track duration via
/// `runtime.core().get_track` and the current volume via
/// `runtime.core().get_playback_state()`.
pub async fn deferred_renderer_join(
    app: &Arc<DaemonQconnectApp>,
    sync_state: &Arc<Mutex<QconnectRemoteSyncState>>,
    runtime: &Runtime,
    volume_mode: VolumeMode, // T10 (OD4): join-time volume report policy
    session_uuid: &str,
    join_reason: i32,
    authority: &AuthorityCell,
    stamp: AuthorityStamp,
) {
    if !authority.is_current(stamp) {
        return;
    }
    let already_joined = {
        let st = sync_state.lock().await;
        if !authority.is_current(stamp) {
            return;
        }
        st.last_joined_session_uuid.as_deref() == Some(session_uuid)
    };
    if already_joined {
        log::info!("[QConnect] Deferred join skipped (session already joined)");
        if !authority.is_current(stamp) {
            return;
        }
        if let Err(err) = app.ask_for_active_renderer_state().await {
            if !authority.is_current(stamp) {
                return;
            }
            log::warn!("[QConnect] Idempotent-join AskForRendererState failed: {err}");
        }
        return;
    }

    let device_info = default_qconnect_device_info();
    if !authority.is_current(stamp) {
        return;
    }

    log::info!("[QConnect] Starting deferred renderer join");

    // 1. Renderer JoinSession with session_uuid.
    // Do NOT auto-steal the render on a fresh connect: join as an AVAILABLE
    // renderer (is_active=false), not the active one. Only a post-drop
    // RECONNECTION rejoins as active, so a network blip mid-render does not lose
    // the render.
    let join_as_active = join_reason == JOIN_SESSION_REASON_RECONNECTION;
    let queue = app.queue_state_snapshot().await;
    let renderer = app.renderer_state_snapshot().await;
    if !authority.is_current(stamp) {
        return;
    }
    let event = if join_as_active {
        runtime.core().player().get_playback_event()
    } else {
        qbz_player::player::PlaybackEvent::default()
    };
    let snapshot = qconnect_app::playback_snapshot_from_event(
        &event,
        &queue,
        renderer
            .current_track
            .as_ref()
            .map(|item| item.queue_item_id),
        renderer.next_track.as_ref().map(|item| item.queue_item_id),
    );
    let queue_version_ref = queue.version;
    let initial_report = qconnect_app::build_renderer_playback_report(
        Uuid::new_v4().to_string(),
        queue.version,
        snapshot,
    );
    let renderer_join_payload = json!({
        "session_uuid": session_uuid,
        "device_info": serde_json::to_value(&device_info).unwrap_or_default(),
        "is_active": join_as_active,
        "reason": join_reason,
        "initial_state": initial_report.payload.clone()
    });
    let renderer_join_report = RendererReport::new(
        RendererReportType::RndrSrvrJoinSession,
        Uuid::new_v4().to_string(),
        queue_version_ref,
        renderer_join_payload,
    );
    if !authority.is_current(stamp) {
        return;
    }
    if let Err(err) = app.send_renderer_report_command(renderer_join_report).await {
        if !authority.is_current(stamp) {
            return;
        }
        log::error!("[QConnect] Deferred renderer join failed: {err}");
        return;
    }

    // Publish the same observation as JoinSession; no fabricated stopped/OK edge.
    let state_report = initial_report;
    if !authority.is_current(stamp) {
        return;
    }
    if let Err(err) = app.send_renderer_report_command(state_report).await {
        if !authority.is_current(stamp) {
            return;
        }
        log::error!("[QConnect] Deferred renderer state report failed: {err}");
    }

    // 3. Report volume and max audio quality.
    // FIX (a) (§7.4, daemon-copy only): report the player's REAL volume, not the
    // desktop's hardcoded 100. `get_playback_state().volume` is a 0.0-1.0
    // fraction; the wire wants a 0-100 percent.
    // T10 (OD4): the volume MODE decides — `Software` reports the real percent,
    // `Locked` reports 100 fixed (no software attenuation on the bit-perfect path).
    let volume_pct = volume_mode.reported_volume_pct(runtime.core().get_playback_state().volume);
    let volume_report = RendererReport::new(
        RendererReportType::RndrSrvrVolumeChanged,
        Uuid::new_v4().to_string(),
        queue_version_ref,
        json!({ "volume": volume_pct }),
    );
    if !authority.is_current(stamp) {
        return;
    }
    if let Err(err) = app.send_renderer_report_command(volume_report).await {
        if !authority.is_current(stamp) {
            return;
        }
        log::error!("[QConnect] Deferred renderer volume report failed: {err}");
    }

    let max_quality_report = RendererReport::new(
        RendererReportType::RndrSrvrMaxAudioQualityChanged,
        Uuid::new_v4().to_string(),
        queue_version_ref,
        json!({ "max_audio_quality": AUDIO_QUALITY_HIRES_LEVEL2 }),
    );
    if !authority.is_current(stamp) {
        return;
    }
    if let Err(err) = app.send_renderer_report_command(max_quality_report).await {
        if !authority.is_current(stamp) {
            return;
        }
        log::error!("[QConnect] Deferred renderer max quality report failed: {err}");
    }

    log::info!("[QConnect] Deferred renderer join complete");

    // Re-request session state so the server sends an updated renderer list
    // (including ourselves).
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    if !authority.is_current(stamp) {
        return;
    }
    let refresh_command = app
        .build_queue_command(QueueCommandType::CtrlSrvrAskForQueueState, json!({}))
        .await;
    if !authority.is_current(stamp) {
        return;
    }
    if let Ok(action_uuid) = app.send_queue_command(refresh_command).await {
        app.clear_pending_if_matches(&action_uuid).await;
        if !authority.is_current(stamp) {
            return;
        }
        log::info!("[QConnect] Re-requested session state after renderer join");
    }

    // Resync the active renderer's full state too, so a reconnect rejoin restores
    // renderer state.
    if !authority.is_current(stamp) {
        return;
    }
    if let Err(err) = app.ask_for_active_renderer_state().await {
        if !authority.is_current(stamp) {
            return;
        }
        log::warn!("[QConnect] Post-join AskForRendererState failed: {err}");
    }

    // Record this session_uuid so a subsequent SESSION_STATE with the same uuid
    // takes the idempotent fast-path above.
    {
        let mut st = sync_state.lock().await;
        if !authority.is_current(stamp) {
            return;
        }
        st.last_joined_session_uuid = Some(session_uuid.to_string());
    }
}
