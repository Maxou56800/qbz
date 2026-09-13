//! Qt QConnect service — the facade (block B3 of the 2026-08-01 QConnect
//! Qt-port contract).
//!
//! Behavior-1:1 port of the Slint `qbz/src/qconnect_service.rs` (which itself
//! reproduces the Tauri `QconnectServiceState`). `QtQconnectService` is the
//! connect-flow facade for the Qt frontend: it owns the connection lifecycle
//! (build transport -> one shared sync-state Mutex -> sink -> `QconnectApp::new`
//! -> `set_app` -> `connect` -> subscribe transport events BEFORE the spawn ->
//! spawn `run_session_loop`), plus controller bootstrap (JoinSession +
//! AskForQueueState), the deferred renderer-join, and every controller-mode
//! `*_if_remote` routing method.
//!
//! `QtSessionLoopHost` implements the frontend-agnostic
//! `qconnect_app::SessionLoopHost` so the shared session loop drives lifecycle,
//! reconnect bootstrap/resync, deferred renderer-join, and reconnect-exhausted
//! teardown through this adapter — exactly as `SlintSessionLoopHost` does.
//!
//! The file exceeds the crate's usual size guidance deliberately: the contract
//! mandates a line-by-line port and splitting it would break the 1:1 mapping
//! (same justification as `qconnect_engine_qt.rs`).
//!
//! The only sanctioned transformations vs the reference:
//! (a) Slint UI pushes (`slint::Weak<AppWindow>` globals) -> the [`publish`]
//!     layer below (B4: each fn routes onto the `QbzQConnect` bridge
//!     singleton via `crate::qconnect_bridge::ui`), EXCEPT the ones wired
//!     directly: toasts (`crate::toast_qt`), is-remote/cast-target
//!     (`crate::now_playing::set_remote`), and remote volume-locked
//!     (`crate::now_playing::set_remote_volume_locked`);
//! (b) the offline engine is `crate::offline_fwd::engine()` (the Qt port of
//!     `crate::offline_mode::engine()`);
//! (c) call-site renames to the Qt runtime accessors (`crate::app()`),
//!     `qconnect_transport_qt`, `qconnect_engine_qt`, `qconnect_event_sink_qt`.
//! `diagnostics_snapshot` / `QconnectDiagSnapshot` feeds Qt's Developer
//! diagnostics panel and bug-report markdown. The autoplay/stop remote APIs
//! remain ported-but-unwired and carry targeted `allow(dead_code)` annotations;
//! they are the only dormant surface in this service.
//!
//! The §11.5 main.rs wiring, the B4 `QbzQConnect` bridge AND the B5/B6
//! `*_if_remote` call sites (playback_qt.rs / queue_qt.rs / playlist_qt.rs /
//! myqbz_play_qt.rs / integrations_qt.rs / cast_qt.rs) are all in place.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard};

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use qbz_player::player::PlaybackBufferState;
use qconnect_app::queue_resolution::{
    find_cursor_index_by_queue_item_id, find_cursor_index_by_track_id, ordered_queue_cursors,
    resolve_controller_queue_item_from_snapshots, resolve_queue_item_ids_from_queue_state,
    QconnectRemoteSkipDirection,
};
use qconnect_app::renderer::{PLAYING_STATE_PAUSED, PLAYING_STATE_PLAYING, PLAYING_STATE_STOPPED};
use qconnect_app::{
    active_peer_renderer_is_playing, arm_local_queue_takeover, build_effective_renderer_snapshot,
    build_renderer_playback_report, confirm_local_playback_state_asserted,
    ensure_session_renderer_state, is_local_renderer_active, is_peer_renderer_active,
    lan_callback_is_current, local_queue_takeover_needs_retry, queue_item_snapshot_for_cursor,
    renderer_allows_remote_volume, set_local_playback_conflict_pending, AuthorityActionPermit,
    AuthorityCell, AuthorityOrigin, AuthorityStamp, DelegationCoordinatorConfig, DelegationHost,
    LanRuntimeLifecycle, LocalPlaybackConflictChoice, OwnerAuthorityObservation,
    OwnerAuthorityToken, QConnectQueueState, QConnectRendererState, QconnectApp, QconnectAppEvent,
    QconnectDisabledToken, QconnectEnableIntent, QconnectEnableToken, QconnectEventSink,
    QconnectFileAudioQualitySnapshot, QconnectLifecycleState, QconnectRemoteSyncState,
    QconnectSessionState, QueueCommandType, RendererPlaybackSnapshot, RendererReport,
    RendererReportType, SessionLoopHost,
};
use qconnect_lan::EndpointPolicy;
use qconnect_transport_ws::{NativeWsTransport, WsTransportConfig};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::qconnect_delegation_qt::{QtDelegationCoordinator, QtDelegationHost};
use crate::qconnect_engine_qt::QtRendererEngine;
use crate::qconnect_event_sink_qt::{QtQconnectApp, QtQconnectEventSink};
use crate::qconnect_lan_qt::{max_audio_quality_for_quality, QtLanProjectionSlot, QtLanRuntime};
use crate::qconnect_transport_qt::{
    build_set_position_player_state_request, default_qconnect_device_info,
    default_qconnect_device_info_with_name, load_persisted_device_name, resolve_transport_config,
    QconnectJoinSessionRequest, QconnectMuteVolumeRequest, QconnectQueueVersionPayload,
    QconnectSetPlayerStateQueueItemPayload, QconnectSetPlayerStateRequest,
    QconnectSetVolumeRequest,
};

const QCONNECT_PLAY_TRACK_HANDOFF_WAIT_MS: u64 = 1_500;
const QCONNECT_PLAY_TRACK_HANDOFF_POLL_MS: u64 = 50;
const QCONNECT_COORDINATOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const QCONNECT_AUTHORITY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Wall-clock now in ms (mirrors the Tauri `qconnect_now_ms`, which is
/// Tauri-local; reimplemented inline here per the controller-port spec).
fn qconnect_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

type Runtime = Arc<AppRuntime<LoggingAdapter>>;

/// QConnect's cloud queue accepts Qobuz catalog ids only. An offline Qobuz
/// download is still the same catalog id and is eligible; every server/file
/// source is not, even when its numeric id happens to look like a Qobuz id.
pub(crate) fn is_qconnect_queue_track(track: &qbz_models::QueueTrack) -> bool {
    qconnect_app::qconnect_queue_track_is_resolvable(track)
}

/// Queue admission follows the user's enabled intent, not the transient
/// transport badge. Otherwise a reconnect window can admit local rows that
/// poison the next cloud sync even though QConnect was never turned off.
pub(crate) fn queue_admission_enabled() -> bool {
    service().is_some_and(|service| service.has_enabled_intent())
}

/// One user-facing notice per queue action, pluralized across all locales.
/// The caller owns batching; this helper never emits once per row.
pub(crate) fn toast_unresolvable_tracks(count: usize) {
    if count == 0 {
        return;
    }
    crate::toast_qt::info(qbz_i18n::tf(
        "Qobuz Connect can't resolve local tracks. {} track was skipped and wasn't added to the queue. Turn off Qobuz Connect to play it.",
        "Qobuz Connect can't resolve local tracks. {} tracks were skipped and weren't added to the queue. Turn off Qobuz Connect to play them.",
        count as i64,
        &[&count.to_string()],
    ));
}

fn resolvable_track_ids(tracks: &[(u64, Option<String>)]) -> (Vec<u64>, usize) {
    let mut kept = Vec::with_capacity(tracks.len());
    let mut dropped = 0;
    for (track_id, source) in tracks {
        if qconnect_app::qconnect_source_is_resolvable(*track_id, source.as_deref()) {
            kept.push(*track_id);
        } else {
            dropped += 1;
        }
    }
    (kept, dropped)
}

/// Defensive projection for queues that predate the admission seam (session
/// restore, an older build, or an internal caller). Keeps catalog order and
/// maps the old cursor onto the first surviving row at/after it, falling back
/// to the last survivor when the remainder of the queue was local-only.
fn resolvable_queue_projection(
    tracks: &[qbz_models::QueueTrack],
    start: Option<usize>,
) -> (Vec<u64>, Option<usize>, usize) {
    let clicked = start.unwrap_or(0);
    let mut kept = Vec::with_capacity(tracks.len());
    let mut new_start = None;
    let mut dropped = 0;
    for (index, track) in tracks.iter().enumerate() {
        if !is_qconnect_queue_track(track) {
            dropped += 1;
            continue;
        }
        if new_start.is_none() && index >= clicked {
            new_start = Some(kept.len());
        }
        kept.push(track.id);
    }
    if new_start.is_none() && !kept.is_empty() {
        new_start = Some(kept.len() - 1);
    }
    (kept, new_start, dropped)
}

/// Publish the local queue while QBZ takes renderer ownership because it is
/// already playing and no peer is. This deliberately bypasses the ordinary
/// `is_local_renderer_active` gate: the SET_ACTIVE command was just sent, but
/// its cloud echo has not landed yet. The upload is still authority-stamped and
/// only contains Qobuz-resolvable rows.
async fn publish_local_queue_for_takeover(
    app: &Arc<QtQconnectApp>,
    sync_state: &Arc<Mutex<QconnectRemoteSyncState>>,
    inner: &Arc<StdMutex<QtQconnectInner>>,
    runtime: &Runtime,
    authority: &AuthorityCell,
    stamp: AuthorityStamp,
) -> bool {
    if !authority.is_current(stamp) || runtime.core().queue_is_offline_only() {
        return false;
    }
    let (tracks, current_index) = runtime.core().get_all_queue_tracks().await;
    if !authority.is_current(stamp) || tracks.is_empty() {
        return false;
    }
    let source_ordered_ids: Vec<u64> = tracks.iter().map(|track| track.id).collect();
    let (ordered_ids, projected_start, dropped) =
        resolvable_queue_projection(&tracks, current_index);
    if dropped > 0 {
        log::info!("[QConnect] Local-playing takeover skipped {dropped} non-Qobuz track(s)");
        toast_unresolvable_tracks(dropped);
    }
    if ordered_ids.is_empty() || !authority.is_current(stamp) {
        lock_inner(inner).last_pushed_queue_ids = Some(source_ordered_ids);
        return false;
    }

    let start_index = projected_start.unwrap_or(0);
    let count = ordered_ids.len();
    let payload = json!({
        "track_ids": ordered_ids.iter().map(|id| *id as i64).collect::<Vec<_>>(),
        "queue_position": start_index,
        "shuffle_mode": false,
        "shuffle_pivot_index": start_index,
        "context_uuid": Uuid::new_v4().to_string(),
        "autoplay_reset": true,
        "autoplay_loading": false,
    });
    let command = app
        .build_queue_command(QueueCommandType::CtrlSrvrQueueLoadTracks, payload)
        .await;
    if !authority.is_current(stamp) {
        return false;
    }
    match app.send_queue_command(command).await {
        Ok(action_uuid) if authority.is_current(stamp) => {
            {
                let mut state = sync_state.lock().await;
                if !authority.is_current(stamp) {
                    return false;
                }
                arm_local_queue_takeover(&mut state, ordered_ids, action_uuid);
                set_local_playback_conflict_pending(&mut state, false);
            }
            lock_inner(inner).last_pushed_queue_ids = Some(source_ordered_ids);
            log::info!(
                "[QConnect] Local playback kept; published filtered queue ({count} tracks, start={start_index})"
            );
            dev_push_event(format!(
                "-> local-playing takeover QueueLoadTracks {count} start={start_index}"
            ));
            true
        }
        Ok(_) => false,
        Err(error) => {
            log::warn!("[QConnect] Local-playing queue publication failed: {error}");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Publish layer (B4: routed onto the `QbzQConnect` bridge singleton)
//
// Everything the Slint adapter pushes onto `QconnectDevState` /
// `NowPlayingState.qconnect-connected` / the dev diagnostics goes through this
// layer, which queues the matching property mutation via
// `crate::qconnect_bridge::ui`. The publishes that do NOT live here: toasts
// go straight to `crate::toast_qt` (msgids per contract §10),
// is-remote/cast-target to `crate::now_playing::set_remote`, remote
// volume-locked to `crate::now_playing::set_remote_volume_locked`, the queue
// UI refresh after `materialize_remote_queue` to
// `crate::queue_qt::publish(runtime)`, and the now-playing meta /
// shuffle-repeat refresh to the same refresh functions `playback_qt.rs` uses.
// ---------------------------------------------------------------------------
pub(crate) mod publish {
    use cxx_qt_lib::QString;

    /// One device-picker row (the Slint `QconnectDevice` struct), serialized
    /// onto `QbzQConnect.devices_json` by [`devices`] (same JSON precedent as
    /// `cast_bridge.rs`'s `devices_json`).
    #[derive(Debug, Clone)]
    pub(crate) struct QconnectDeviceRow {
        pub renderer_id: i32,
        pub name: String,
        pub is_local: bool,
        pub is_active: bool,
        /// Icon key from `device_icon_key`: "mobile" | "web" | "computer" |
        /// "speaker".
        pub icon: &'static str,
    }

    /// `NowPlayingState.qconnect-connected` -> `QbzQConnect.qconnect_connected`
    /// (the golden ConnectButton badge).
    pub(crate) fn connected(connected: bool) {
        crate::qconnect_bridge::ui(move |mut b| {
            b.as_mut().set_qconnect_connected(connected);
        });
    }

    /// `QconnectDevState.devices` -> `QbzQConnect.devices_json`. Rebuilt on
    /// every inbound event by the sink. The wire shape QML parses (exactly
    /// these keys — the full contract is documented in qconnect_bridge.rs):
    /// `[{ renderer_id, name, is_local, is_active, icon }]`.
    pub(crate) fn devices(rows: Vec<QconnectDeviceRow>) {
        let json = serde_json::to_string(
            &rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "renderer_id": row.renderer_id,
                        "name": row.name,
                        "is_local": row.is_local,
                        "is_active": row.is_active,
                        "icon": row.icon,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".to_string());
        crate::qconnect_bridge::ui(move |mut b| {
            b.as_mut().set_devices_json(QString::from(json.as_str()));
        });
    }

    /// `QconnectDevState.active-renderer-id` -> `QbzQConnect.active_renderer_id`
    /// (-1 = none).
    pub(crate) fn active_renderer_id(renderer_id: i32) {
        crate::qconnect_bridge::ui(move |mut b| {
            b.as_mut().set_active_renderer_id(renderer_id);
        });
    }

    /// Open/close the global playback-conflict modal. The session loop remains
    /// fenced while it is open, so this publish is presentation only.
    pub(crate) fn playback_conflict(open: bool, renderer_name: String) {
        crate::qconnect_bridge::ui(move |mut b| {
            b.as_mut()
                .set_playback_conflict_renderer_name(QString::from(renderer_name.as_str()));
            b.as_mut().set_playback_conflict_open(open);
        });
    }

    /// Shared persisted conflict-policy selection. The flyout binds this
    /// property directly; Settings receives the same index in its snapshot.
    pub(crate) fn playback_conflict_policy_index(index: i32) {
        crate::qconnect_bridge::ui(move |mut b| {
            b.as_mut().set_playback_conflict_policy_index(index);
        });
    }

    /// `QconnectDevState.status` -> `QbzQConnect.diag_status` (the diagnostics
    /// modal's live status block).
    pub(crate) fn diag_status(status: String) {
        crate::qconnect_bridge::ui(move |mut b| {
            b.as_mut().set_diag_status(QString::from(status.as_str()));
        });
    }

    /// `QconnectDevState.log-text` -> `QbzQConnect.diag_log_text` (the rolling
    /// 150-line event log, newest first).
    pub(crate) fn diag_log(text: String) {
        crate::qconnect_bridge::ui(move |mut b| {
            b.as_mut().set_diag_log_text(QString::from(text.as_str()));
        });
    }

    /// `QconnectDevState.clear()` side effect -> the bridge's diag log reset.
    pub(crate) fn diag_clear() {
        crate::qconnect_bridge::ui(move |mut b| {
            b.as_mut().set_diag_log_text(QString::default());
        });
    }
}

/// Reduced peer-renderer playback snapshot for the now-playing seek bar while
/// QBZ is CONTROLLING a peer. Sourced from the effective remote renderer
/// snapshot; the poll loop extrapolates position from `position_ms` +
/// (now - `updated_at_ms`) while `playing`. Avoids leaking core types into the
/// playback module. Mirrors the Svelte `effectiveCurrentTime` derivation.
pub struct RemoteNowPlaying {
    pub position_ms: u64,
    pub updated_at_ms: u64,
    pub playing: bool,
    /// Peer renderer's reported volume (0..=100). `None` when the peer hasn't
    /// reported a volume yet — the bar then clamps to a safe 50% instead of
    /// reflecting QBZ's local 100, so a drag never nukes the AVR.
    pub volume: Option<i32>,
    /// Peer mute state; independent of the owner's saved local mute toggle.
    pub muted: bool,
    /// The peer's current track id (from the effective remote renderer
    /// snapshot's `current_track`; 0 when none). The poll loop edge-detects a
    /// change against its last-seen value to refresh the bar/queue meta when
    /// the peer advances a track on its own.
    pub track_id: u64,
    /// The peer's shuffle flag, so the controller bar's shuffle button reflects
    /// the REMOTE state (the poll loop only updated this for local playback).
    pub shuffle_mode: bool,
    /// The peer's repeat mode, already mapped to the UI's `repeat-mode`
    /// (0=off, 1=all, 2=one) from the QConnect wire loop_mode (1=off, 3=all,
    /// 2=one), so the controller bar's repeat button reflects the REMOTE state.
    pub repeat_mode: i32,
}

fn peer_seek_position_ms(
    fraction: f32,
    track_id: u64,
    tracks: &[qbz_models::QueueTrack],
) -> Option<i64> {
    let duration_secs = tracks
        .iter()
        .find(|track| track.id == track_id)
        .map(|track| track.duration_secs)
        .filter(|duration| *duration > 0)?;
    if !fraction.is_finite() {
        return None;
    }
    let position_ms = (fraction.clamp(0.0, 1.0) as f64 * duration_secs as f64 * 1000.0).round();
    (position_ms <= i32::MAX as f64).then_some(position_ms as i64)
}

fn project_peer_seek(
    sync: &mut QconnectRemoteSyncState,
    renderer_id: i32,
    queue_item_id: u64,
    position_ms: u64,
    updated_at_ms: u64,
) -> bool {
    if sync.session.active_renderer_id != Some(renderer_id)
        || !is_peer_renderer_active(&sync.session)
    {
        return false;
    }
    let Some(renderer) = sync.session_renderer_states.get_mut(&renderer_id) else {
        return false;
    };
    if renderer.current_queue_item_id != Some(queue_item_id) {
        return false;
    }
    renderer.current_position_ms = Some(position_ms);
    renderer.updated_at_ms = updated_at_ms;
    true
}

fn project_peer_mute(sync: &mut QconnectRemoteSyncState, renderer_id: i32, muted: bool) -> bool {
    if sync.session.active_renderer_id != Some(renderer_id)
        || !is_peer_renderer_active(&sync.session)
    {
        return false;
    }
    // Mute does not establish a new playback-position anchor.
    ensure_session_renderer_state(sync, renderer_id).muted = Some(muted);
    true
}

fn active_renderer_projection(
    queue: &QConnectQueueState,
    local_renderer: &QConnectRendererState,
    sync: &QconnectRemoteSyncState,
) -> QConnectRendererState {
    let cached = sync
        .session
        .active_renderer_id
        .and_then(|id| sync.session_renderer_states.get(&id));
    if is_peer_renderer_active(&sync.session) {
        // Local renderer commands describe THIS device, never the active peer.
        // Missing/-1/unresolvable peer cursors must not inherit a stale local
        // track, playback state, volume or mute and send controls to that item.
        qconnect_app::build_session_renderer_snapshot(queue, cached, sync.session_loop_mode)
    } else {
        build_effective_renderer_snapshot(queue, local_renderer, cached, sync.session_loop_mode)
    }
}

/// Process-wide QConnect service singleton (one per app, like the playback
/// QueueController). Initialized once at shell setup; the connect trigger + the
/// future `*_if_remote` transport routing reach it through `service()`.
static SERVICE: std::sync::OnceLock<Arc<QtQconnectService>> = std::sync::OnceLock::new();

type PlaybackConflictSender = tokio::sync::oneshot::Sender<LocalPlaybackConflictChoice>;
static PLAYBACK_CONFLICT_SENDER: std::sync::OnceLock<StdMutex<Option<PlaybackConflictSender>>> =
    std::sync::OnceLock::new();

fn playback_conflict_sender() -> &'static StdMutex<Option<PlaybackConflictSender>> {
    PLAYBACK_CONFLICT_SENDER.get_or_init(|| StdMutex::new(None))
}

async fn request_playback_conflict_choice(renderer_name: String) -> LocalPlaybackConflictChoice {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let previous = playback_conflict_sender()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .replace(sender);
    drop(previous);
    publish::playback_conflict(true, renderer_name);
    receiver
        .await
        .unwrap_or(LocalPlaybackConflictChoice::CancelConnection)
}

/// QML result for the conflict modal. Invalid values fail closed as Cancel.
pub(crate) fn resolve_playback_conflict_choice(choice: i32) {
    let choice = match choice {
        1 => LocalPlaybackConflictChoice::ContinueOnActiveRenderer,
        2 => LocalPlaybackConflictChoice::ContinueOnThisDevice,
        3 => LocalPlaybackConflictChoice::ContinueLocalPlaybackAndReplaceQueue,
        4 => LocalPlaybackConflictChoice::CancelConnection,
        value => {
            log::warn!("[QConnect] invalid playback-conflict choice: {value}");
            LocalPlaybackConflictChoice::CancelConnection
        }
    };
    publish::playback_conflict(false, String::new());
    let sender = playback_conflict_sender()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(sender) = sender {
        let _ = sender.send(choice);
    }
}

/// Initialize the QConnect service singleton (idempotent — a second call returns
/// the existing instance, ignoring the new args).
pub fn init_service(runtime: Runtime) -> Arc<QtQconnectService> {
    SERVICE
        .get_or_init(|| Arc::new(QtQconnectService::new(runtime)))
        .clone()
}

/// The initialized QConnect service, if shell setup has run.
pub fn service() -> Option<Arc<QtQconnectService>> {
    SERVICE.get().cloned()
}

/// Startup auto-connect (Settings > Playback, "Auto-connect Qobuz Connect on
/// startup"). Ports the Tauri `startup.rs::maybe_auto_connect_after_bootstrap`
/// + its bounded retry schedule (gap #8): decide from the persisted mode (and,
/// for RememberLast, the last-known connect state), then drive the SAME
/// `connect()` path as the bar toggle — no duplicated connection logic.
///
/// Called from `on_session_entered` AFTER session activation, online sessions
/// only (the offline shell entry never calls this); `connect()` itself
/// additionally refuses an uninitialized API and offline mode (D5). Fires at
/// most once per process, so a logout/login cycle does not re-trigger it.
pub fn spawn_startup_auto_connect(handle: &tokio::runtime::Handle) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FIRED: AtomicBool = AtomicBool::new(false);
    if FIRED.swap(true, Ordering::SeqCst) {
        return;
    }
    handle.spawn(async move {
        // Blocking SQLite reads — cheap, but keep them off the caller's path.
        let mode = crate::qconnect_transport_qt::load_startup_mode();
        let last = crate::qconnect_transport_qt::load_last_known_state();
        let should_connect = qconnect_app::compute_effective_startup(mode, None, last);
        log::info!(
            "[QConnect] startup auto-connect decision: mode={} last_known={:?} -> {}",
            mode.as_str(),
            last,
            should_connect
        );
        if !should_connect {
            return;
        }
        // The service singleton is initialized during shell-entry wiring, which
        // runs concurrently with the session-restore task that enters the
        // shell — wait for it briefly instead of racing (it lands within
        // milliseconds).
        let service = {
            let mut waited_ms = 0u64;
            loop {
                if let Some(svc) = service() {
                    break svc;
                }
                if waited_ms >= 5_000 {
                    log::warn!("[QConnect] startup auto-connect: service never initialized");
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                waited_ms += 100;
            }
        };
        let enable_token = service.enable_intent.enable();
        // Tauri's `startup_retry_schedule()` (gap #8): one immediate attempt,
        // then one per scheduled delay; give up for the session after the last
        // step. Each connect() re-resolves the transport config internally, so
        // a transient credential/network failure can clear on a later attempt.
        let schedule: [u64; 4] = [2_000, 5_000, 15_000, 30_000];
        for attempt in 0..=schedule.len() {
            if !service.enable_intent.is_current(enable_token) {
                log::info!("[QConnect] startup auto-connect cancelled by disable");
                return;
            }
            // D5: offline (incl. persisted induced-offline surviving a
            // restart) — bail silently instead of letting connect()'s
            // offline refusal TOAST on every retry (5 spams over ~52s).
            if crate::offline_fwd::engine().is_offline() {
                log::info!("[QConnect] startup auto-connect skipped: offline mode active (D5)");
                return;
            }
            match service.connect_with_token(enable_token).await {
                Ok(()) => {
                    if service
                        .enable_intent
                        .commit_if_current(enable_token, || {
                            log::info!("[QConnect] startup auto-connect succeeded");
                            // Mirror the manual toggle's tail: flip the bar badge on.
                            publish::connected(true);
                        })
                        .is_none()
                    {
                        return;
                    }
                    return;
                }
                Err(err) => {
                    if !service.enable_intent.is_current(enable_token) {
                        return;
                    }
                    log::warn!(
                        "[QConnect] startup auto-connect attempt {} failed: {err}",
                        attempt + 1
                    );
                }
            }
            match schedule.get(attempt) {
                Some(delay_ms) => {
                    if !service.enable_intent.is_current(enable_token) {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(*delay_ms)).await;
                    if !service.enable_intent.is_current(enable_token) {
                        return;
                    }
                }
                None => {
                    log::warn!(
                        "[QConnect] startup auto-connect gave up for this session after {} attempts",
                        attempt + 1
                    );
                    return;
                }
            }
        }
    });
}

// ---- DEV diagnostics (QconnectDevModal) ------------------------------------
// A rolling, runtime-inspectable event log + live status block, so QConnect can
// be debugged WITHOUT a rebuild (Slint builds are slow). Populated by the event
// sink; rendered by the Qt `qml/shell/QconnectDevModal.qml` (block B4). The
// state lives HERE in the facade (1:1 with the reference); the publishes go
// through the `publish` shell.

static DEV_LOG: std::sync::OnceLock<std::sync::Mutex<std::collections::VecDeque<String>>> =
    std::sync::OnceLock::new();
static DEV_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
const DEV_LOG_CAP: usize = 150;

fn dev_log_text(push: Option<String>, clear: bool) -> String {
    let buf = DEV_LOG.get_or_init(|| std::sync::Mutex::new(std::collections::VecDeque::new()));
    let mut guard = buf.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if clear {
        guard.clear();
    }
    if let Some(line) = push {
        guard.push_front(line);
        while guard.len() > DEV_LOG_CAP {
            guard.pop_back();
        }
    }
    guard.iter().cloned().collect::<Vec<_>>().join("\n")
}

/// Append a formatted event line (with a relative timestamp) to the DEV log and
/// push the joined text to the modal. Called for every inbound QConnect event.
pub fn dev_push_event(line: String) {
    let start = DEV_START.get_or_init(std::time::Instant::now);
    let ms = start.elapsed().as_millis();
    let text = dev_log_text(Some(format!("[{ms}ms] {line}")), false);
    publish::diag_log(text);
}

/// Replace the DEV status block (session topology / renderer roles / queue).
pub fn dev_set_status(status: String) {
    publish::diag_status(status);
}

/// Clear the DEV event log (wired to `QconnectDevState.clear()` /
/// `QbzQConnect.diag_clear()`).
pub fn dev_clear() {
    let _ = dev_log_text(None, true);
    publish::diag_clear();
}

pub(crate) struct QtQconnectRuntime {
    pub(crate) stamp: AuthorityStamp,
    pub(crate) app: Arc<QtQconnectApp>,
    // Read by `diagnostics_snapshot` (the "Has Endpoint" row).
    pub(crate) config: WsTransportConfig,
    pub(crate) event_loop: tokio::task::JoinHandle<()>,
    // Shared with the app + sink; read by the renderer snapshots and by
    // `diagnostics_snapshot` (session topology).
    pub(crate) sync_state: Arc<Mutex<QconnectRemoteSyncState>>,
}

#[derive(Default)]
pub(crate) struct QtQconnectInner {
    pub(crate) runtime: Option<QtQconnectRuntime>,
    pub(crate) last_error: Option<String>,
    pub(crate) lifecycle_state: QconnectLifecycleState,
    /// Exact enabled intent that owns the pre-runtime `Connecting` claim.
    /// A disable can clear it synchronously, and a late attempt can therefore
    /// never release or overwrite a newer attempt's claim.
    pub(crate) connecting_token: Option<QconnectEnableToken>,
    pub(crate) last_pushed_queue_ids: Option<Vec<u64>>,
}

pub(crate) fn lock_inner(inner: &StdMutex<QtQconnectInner>) -> StdMutexGuard<'_, QtQconnectInner> {
    inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Dedup + gate a lifecycle transition: only emit while a runtime is alive and
/// the state actually changes. Mirrors the Tauri `update_lifecycle_state_if_running`.
async fn update_lifecycle_state_if_running(
    inner: &Arc<StdMutex<QtQconnectInner>>,
    sink: &QtQconnectEventSink,
    authority: &AuthorityCell,
    stamp: AuthorityStamp,
    next: QconnectLifecycleState,
) {
    if !authority.is_current(stamp) {
        return;
    }
    {
        let mut guard = lock_inner(inner);
        if guard.runtime.as_ref().map(|runtime| runtime.stamp) != Some(stamp)
            || guard.lifecycle_state == next
        {
            return;
        }
        guard.lifecycle_state = next;
    }
    if !authority.is_current(stamp) {
        return;
    }
    sink.on_event(QconnectAppEvent::LifecycleChanged { state: next })
        .await;
}

pub struct QtQconnectService {
    inner: Arc<StdMutex<QtQconnectInner>>,
    authority: Arc<AuthorityCell>,
    enable_intent: Arc<QconnectEnableIntent>,
    runtime: Runtime,
    custom_device_name: Arc<tokio::sync::RwLock<Option<String>>>,
    delegation_host: Arc<QtDelegationHost>,
    coordinator: QtDelegationCoordinator,
    lan: Mutex<Option<QtLanRuntime>>,
    lan_lifecycle: LanRuntimeLifecycle<QtLanRuntime>,
    lifecycle_gate: Mutex<()>,
    /// Controller-mode mirror of the local queue's manual block (#442 "Play
    /// later"): steers `insert_after` for play-later routing. See the struct
    /// docs on `ControllerManualBlock`.
    controller_manual: Mutex<ControllerManualBlock>,
    teardown_incomplete: AtomicBool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct QconnectDisconnectOutcome {
    pub authority_safe: bool,
    pub owner_restored: bool,
    pub lan_withdrawn: bool,
    pub cast_restore_token: Option<QconnectDisabledToken>,
}

/// Result of routing a Queue View insertion to the active peer. The historical
/// bool API intentionally treats both a refusal and a send failure as handled
/// (never mutate the local queue while a peer owns playback); History needs the
/// finer answer so it only retires its local occurrence after a confirmed send.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PeerInsertAtSlotOutcome {
    Inactive,
    Inserted,
    HandledFailure,
}

/// Controller-mode mirror of the local queue's manual block (#442 "Play later").
///
/// The QConnect protocol has no "later" concept and the peer's queue carries no
/// manual-block metadata; worse, every cloud echo re-materializes the LOCAL core
/// queue via `materialize_remote_queue`, resetting its `manual_next_count` — so
/// the block tail is tracked HERE instead. Every play-next / play-later routed
/// to the peer is recorded as a PENDING insert (anchor + track ids); the next
/// `reconcile` confirms it against the fresh snapshot once the echo lands (the
/// inserted ids show up after the anchor we used). The tail is dropped when the
/// renderer's current track advances at/past it, and everything resets when the
/// queue's major version changes (a wholesale replace wipes the block).
/// Best-effort by design: it only steers `insert_after`; the cloud remains the
/// source of truth.
#[derive(Default)]
struct ControllerManualBlock {
    /// queue_item_id of the deepest confirmed manual item (the block tail).
    tail_qid: Option<u64>,
    /// Renderer current-track qid at the last reconcile (advance detection).
    current_qid: Option<u64>,
    /// Queue major version at the last reconcile (replace detection).
    queue_major: Option<u64>,
    /// Sent-but-unconfirmed inserts: (insert_after anchor we used, track ids).
    pending: Vec<(u64, Vec<u64>)>,
}

impl ControllerManualBlock {
    /// Reconcile with the latest peer snapshot BEFORE computing a new anchor.
    fn reconcile(&mut self, renderer: &QConnectRendererState, queue: &QConnectQueueState) {
        let items = &queue.queue_items;

        // Queue replaced wholesale → the block is gone.
        if self.queue_major != Some(queue.version.major) {
            self.queue_major = Some(queue.version.major);
            self.current_qid = renderer.current_track.as_ref().map(|i| i.queue_item_id);
            self.tail_qid = None;
            self.pending.clear();
            return;
        }

        // Advance detection: the current track moving consumes manual items.
        // The tail only survives while it sits strictly AFTER the current track.
        let new_current = renderer.current_track.as_ref().map(|i| i.queue_item_id);
        if new_current != self.current_qid {
            if let (Some(tail), Some(cur)) = (self.tail_qid, new_current) {
                let tail_pos = items.iter().position(|i| i.queue_item_id == tail);
                let cur_pos = items.iter().position(|i| i.queue_item_id == cur);
                let tail_still_ahead = matches!((tail_pos, cur_pos), (Some(t), Some(c)) if t > c);
                if !tail_still_ahead {
                    self.tail_qid = None;
                }
            } else {
                self.tail_qid = None;
            }
            self.current_qid = new_current;
        }

        // Confirm pendings against the snapshot: the echo has landed once the
        // inserted ids show up shortly after the anchor we used. The small scan
        // window tolerates a play-next interleaving between two play-laters
        // (which pushes an earlier play-later one position deeper).
        let mut confirmed: Vec<u64> = Vec::new();
        self.pending.retain(|(anchor, ids)| {
            let Some(anchor_pos) = items.iter().position(|i| i.queue_item_id == *anchor) else {
                return false; // anchor gone — the queue moved on; drop.
            };
            let window_end = (anchor_pos + 1 + ids.len() + 2).min(items.len());
            let window = &items[anchor_pos + 1..window_end];
            match ids.last() {
                Some(last_id) => match window.iter().find(|i| i.track_id == *last_id) {
                    Some(item) => {
                        confirmed.push(item.queue_item_id);
                        false // confirmed → drop from pending
                    }
                    None => true, // echo not here yet
                },
                None => false,
            }
        });
        // Raise the tail to the DEEPEST confirmed item — never lower it (two
        // rapid inserts confirm in send order, but the second anchor sits
        // shallower when its echo landed first).
        let deepest = confirmed
            .into_iter()
            .filter_map(|qid| {
                items
                    .iter()
                    .position(|i| i.queue_item_id == qid)
                    .map(|pos| (pos, qid))
            })
            .max_by_key(|(pos, _)| *pos);
        if let Some((pos, qid)) = deepest {
            let raises = self
                .tail_qid
                .and_then(|tail| items.iter().position(|i| i.queue_item_id == tail))
                .map(|tail_pos| pos > tail_pos)
                .unwrap_or(true);
            if raises {
                self.tail_qid = Some(qid);
            }
        }
    }

    /// queue_item_id AFTER which a play-later insert lands: the confirmed block
    /// tail while it is still ahead of the current track, else the current
    /// track itself (degrades to play-next — also under shuffle, mirroring the
    /// local `add_track_later` shuffle fallback).
    fn later_anchor(
        &self,
        renderer: &QConnectRendererState,
        queue: &QConnectQueueState,
    ) -> Option<u64> {
        let current = renderer.current_track.as_ref()?.queue_item_id;
        if queue.shuffle_mode {
            return Some(current);
        }
        let items = &queue.queue_items;
        if let Some(tail) = self.tail_qid {
            let cur_pos = items.iter().position(|i| i.queue_item_id == current);
            let tail_pos = items.iter().position(|i| i.queue_item_id == tail);
            if let (Some(c), Some(t)) = (cur_pos, tail_pos) {
                if t > c {
                    return Some(tail);
                }
            }
        }
        Some(current)
    }

    /// Record a just-sent insert so the next reconcile can confirm its echo.
    fn note_sent(
        &mut self,
        renderer: &QConnectRendererState,
        queue: &QConnectQueueState,
        anchor: Option<u64>,
        ids: Vec<u64>,
    ) {
        // Seed the queue generation before recording the first Play next batch;
        // otherwise the first Play later reconcile discards it as a new queue.
        self.reconcile(renderer, queue);
        if let (Some(anchor), false) = (anchor, ids.is_empty()) {
            self.pending.push((anchor, ids));
        }
    }
}

/// Qt-local mirror of the Tauri `QconnectVisibleQueueProjection` reduced to
/// the pieces the reorder payload needs: the current track's queue_item_id (the
/// anchor) and the ordered upcoming queue_item_ids. Built from the cloud queue +
/// renderer snapshot via the shared `qconnect-app` cursor helpers (no Tauri-local
/// code). Stores only ids so it needs no `qconnect-core::QueueItem` import.
struct VisibleUpcomingProjection {
    current_track_qid: Option<u64>,
    upcoming_qids: Vec<u64>,
}

/// Rebuild the visible upcoming projection (current anchor + ordered upcoming
/// queue_item_ids) from a cloud queue + renderer snapshot, mirroring the Tauri
/// `build_visible_queue_projection` using the SHARED qconnect-app cursor helpers.
fn build_visible_upcoming_projection(
    queue: &QConnectQueueState,
    renderer: &QConnectRendererState,
) -> VisibleUpcomingProjection {
    let cursors = ordered_queue_cursors(queue);

    let current_index = find_cursor_index_by_queue_item_id(
        &cursors,
        queue,
        renderer.current_track.as_ref().map(|i| i.queue_item_id),
    )
    .or_else(|| {
        find_cursor_index_by_track_id(
            &cursors,
            queue,
            renderer.current_track.as_ref().map(|i| i.track_id),
        )
    });

    let next_index = find_cursor_index_by_queue_item_id(
        &cursors,
        queue,
        renderer.next_track.as_ref().map(|i| i.queue_item_id),
    )
    .or_else(|| {
        find_cursor_index_by_track_id(
            &cursors,
            queue,
            renderer.next_track.as_ref().map(|i| i.track_id),
        )
    });

    // (current_track, start_index): the upcoming list starts AFTER the current
    // track; if the current is unknown but the next is, infer the current from
    // the cursor before next. Mirrors the Tauri projection.
    let (current_track_qid, start_index) = if let Some(index) = current_index {
        (
            queue_item_snapshot_for_cursor(queue, cursors[index]).map(|i| i.queue_item_id),
            index.saturating_add(1),
        )
    } else if let Some(index) = next_index {
        let inferred = index
            .checked_sub(1)
            .and_then(|c| cursors.get(c).copied())
            .and_then(|cur| queue_item_snapshot_for_cursor(queue, cur))
            .map(|i| i.queue_item_id);
        (inferred, index)
    } else {
        (None, 0)
    };

    let upcoming_qids = cursors
        .into_iter()
        .skip(start_index)
        .filter_map(|cur| queue_item_snapshot_for_cursor(queue, cur))
        .map(|i| i.queue_item_id)
        .collect();

    VisibleUpcomingProjection {
        current_track_qid,
        upcoming_qids,
    }
}

fn remote_upcoming_selection(
    queue: &QConnectQueueState,
    renderer: &QConnectRendererState,
    upcoming_index: usize,
    expected_track_id: u64,
) -> Option<QconnectSetPlayerStateRequest> {
    if queue.shuffle_mode
        && !queue.shuffle_order.as_ref().is_some_and(|order| {
            qconnect_app::queue_resolution::is_valid_ordered_queue_shuffle_order(
                order,
                queue.queue_items.len(),
            )
        })
    {
        return None;
    }
    let projection = build_visible_upcoming_projection(queue, renderer);
    let target_qid = *projection.upcoming_qids.get(upcoming_index)?;
    let target = ordered_queue_cursors(queue)
        .into_iter()
        .filter_map(|cursor| queue_item_snapshot_for_cursor(queue, cursor))
        .find(|item| item.queue_item_id == target_qid)?;
    if target.track_id != expected_track_id {
        return None;
    }
    Some(QconnectSetPlayerStateRequest {
        playing_state: Some(PLAYING_STATE_PLAYING),
        current_position: Some(0),
        current_queue_item: Some(QconnectSetPlayerStateQueueItemPayload {
            queue_version: Some(QconnectQueueVersionPayload {
                major: queue.version.major,
                minor: queue.version.minor,
            }),
            id: Some(i32::try_from(target_qid).ok()?),
        }),
    })
}

fn local_upcoming_matches_remote(
    queue: &QConnectQueueState,
    renderer: &QConnectRendererState,
    tracks: &[qbz_models::QueueTrack],
    local: &qbz_models::QueueState,
) -> bool {
    // Hydration can omit unavailable rows. Do not shift an index silently,
    // especially when multiple queue occurrences have the same catalog id.
    if !tracks.iter().all(is_qconnect_queue_track)
        || !tracks
            .iter()
            .map(|track| track.id)
            .eq(queue.queue_items.iter().map(|item| item.track_id))
        || local.shuffle != queue.shuffle_mode
    {
        return false;
    }
    let Some(current_qid) = renderer
        .current_track
        .as_ref()
        .map(|item| item.queue_item_id)
    else {
        return false;
    };
    let Some(current_index) = queue.queue_items.iter().enumerate().position(|(index, _)| {
        qconnect_app::queue_resolution::normalize_current_queue_item_id_from_queue_state(
            queue, index,
        ) == current_qid
    }) else {
        return false;
    };
    if local.current_index != Some(current_index) {
        return false;
    }
    let projection = build_visible_upcoming_projection(queue, renderer);
    let main_by_qid: std::collections::HashMap<_, _> = queue
        .queue_items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            (
                qconnect_app::queue_resolution::normalize_current_queue_item_id_from_queue_state(
                    queue, index,
                ),
                item.track_id,
            )
        })
        .collect();
    if main_by_qid.len() != queue.queue_items.len() {
        return false;
    }
    let main_upcoming_ids = projection
        .upcoming_qids
        .iter()
        .filter_map(|qid| main_by_qid.get(qid).copied());
    local
        .upcoming
        .iter()
        .map(|track| track.id)
        .eq(main_upcoming_ids)
}

/// 1:1 port of the Tauri `build_qconnect_reorder_payload`. `from_index`/
/// `to_index` index INTO the visible upcoming list. None when out of range,
/// `Some({})` for a no-op, else the wire payload (moved id + insert_after anchor).
fn build_reorder_payload(
    projection: &VisibleUpcomingProjection,
    from_index: usize,
    to_index: usize,
) -> Option<Value> {
    let len = projection.upcoming_qids.len();
    if from_index >= len || to_index >= len {
        return None;
    }
    if from_index == to_index {
        return Some(json!({}));
    }

    let mut ids = projection.upcoming_qids.clone();
    let moved = ids.remove(from_index);
    let insert_position = if from_index < to_index {
        to_index.saturating_sub(1)
    } else {
        to_index
    };
    let insert_after = if insert_position == 0 {
        projection.current_track_qid
    } else {
        ids.get(insert_position - 1).copied()
    };

    Some(json!({
        "queue_item_ids": [moved as i64],
        "insert_after": insert_after.map(|v| v as i64),
        "autoplay_reset": false,
        "autoplay_loading": false,
    }))
}

/// Pure observation of the live session, for the Settings > Developer
/// Diagnostics panel and the log bundle's markdown report.
///
/// Ported from `crates/qbz/src/qconnect_service.rs:572` (§9 D10 used to say
/// this was SKIPPED "because its only Slint consumers do not exist in Qt" —
/// they exist as of 2026-08-14, so it is ported).
///
/// Every field is a PURE read of state the running service already maintains
/// (lock -> read -> clone -> unlock). It never touches discovery, guard or
/// transport logic — QConnect is control-fragile, so this only observes.
/// `last_error` is returned raw; the diagnostics controller redacts id-like
/// substrings before it reaches the UI or the export.
#[derive(Default)]
pub struct QconnectDiagSnapshot {
    pub running: bool,
    pub transport_connected: bool,
    pub has_endpoint: bool,
    pub last_error: Option<String>,
    pub role: &'static str,
    pub active_name: Option<String>,
    pub active_brand: Option<String>,
    pub active_model: Option<String>,
    pub renderer_count: usize,
}

impl QtQconnectService {
    /// See [`QconnectDiagSnapshot`]. Returns a default snapshot (role "none")
    /// when the service is not running.
    pub(crate) async fn diagnostics_snapshot(&self) -> QconnectDiagSnapshot {
        // Pull the app + sync-state handles (and the static fields) under a
        // brief inner lock, then release it before awaiting the per-handle
        // locks — the reference's ordering, kept verbatim.
        let (app, sync_state, last_error, has_endpoint, running) = {
            let guard = lock_inner(&self.inner);
            match guard.runtime.as_ref() {
                Some(rt) => (
                    Some(Arc::clone(&rt.app)),
                    Some(Arc::clone(&rt.sync_state)),
                    guard.last_error.clone(),
                    !rt.config.endpoint_url.is_empty(),
                    true,
                ),
                None => (None, None, guard.last_error.clone(), false, false),
            }
        };

        let mut snap = QconnectDiagSnapshot {
            running,
            has_endpoint,
            last_error,
            role: "none",
            ..Default::default()
        };

        let (Some(app), Some(sync_state)) = (app, sync_state) else {
            return snap;
        };

        // Transport-connected mirror (same source the event sink reads).
        snap.transport_connected = app.state_handle().lock().await.transport_connected;

        // Session topology: renderer count + the active renderer's identity.
        let state = sync_state.lock().await;
        let session = &state.session;
        snap.renderer_count = session.renderers.len();
        let active_id = session.active_renderer_id;
        let local_id = session.local_renderer_id;
        if let Some(active_id) = active_id {
            if let Some(renderer) = session
                .renderers
                .iter()
                .find(|r| r.renderer_id == active_id)
            {
                snap.active_name = renderer.friendly_name.clone();
                snap.active_brand = renderer.brand.clone();
                snap.active_model = renderer.model.clone();
            }
            // active == local -> we render locally; active != local -> we
            // control a peer. Mirrors the Tauri role rule.
            snap.role = if Some(active_id) == local_id {
                "local-renderer"
            } else {
                "controller"
            };
        } else if snap.running || snap.transport_connected {
            snap.role = "observer";
        } else {
            snap.role = "none";
        }

        snap
    }
}

impl QtQconnectService {
    pub fn new(runtime: Runtime) -> Self {
        let saved_name = load_persisted_device_name();
        let inner = Arc::new(StdMutex::new(QtQconnectInner::default()));
        let authority = Arc::new(AuthorityCell::new());
        let enable_intent = Arc::new(QconnectEnableIntent::new(false));
        let custom_device_name = Arc::new(tokio::sync::RwLock::new(saved_name));
        let delegation_host = Arc::new(QtDelegationHost::new(
            Arc::clone(&runtime),
            Arc::clone(&inner),
            Arc::clone(&custom_device_name),
            Arc::clone(&authority),
        ));
        let coordinator = QtDelegationCoordinator::disabled(
            Arc::clone(&delegation_host),
            DelegationCoordinatorConfig::default(),
        );
        assert!(
            delegation_host.install_coordinator(coordinator.clone()),
            "QConnect delegation coordinator may only be installed once"
        );
        Self {
            inner,
            authority,
            enable_intent,
            runtime,
            custom_device_name,
            delegation_host,
            coordinator,
            lan: Mutex::new(None),
            lan_lifecycle: LanRuntimeLifecycle::new(|runtime: &mut QtLanRuntime| {
                runtime.shutdown_blocking()
            }),
            lifecycle_gate: Mutex::new(()),
            controller_manual: Mutex::new(ControllerManualBlock::default()),
            teardown_incomplete: AtomicBool::new(false),
        }
    }

    async fn await_while_enabled<T>(
        &self,
        enable_token: QconnectEnableToken,
        future: impl std::future::Future<Output = T>,
    ) -> Result<T, String> {
        let value = tokio::select! {
            biased;
            _ = self.enable_intent.cancelled(enable_token) => {
                return Err("Qobuz Connect was disabled".to_string());
            }
            value = future => value,
        };
        self.enable_intent
            .is_current(enable_token)
            .then_some(value)
            .ok_or_else(|| "Qobuz Connect was disabled".to_string())
    }

    async fn await_connect_handoff<T>(
        &self,
        enable_token: QconnectEnableToken,
        cast_transition: &crate::cast_qt::CastQconnectTransitionLease,
        future: impl std::future::Future<Output = T>,
    ) -> Result<T, String> {
        let value = tokio::select! {
            biased;
            _ = self.enable_intent.cancelled(enable_token) => {
                return Err("Qobuz Connect was disabled".to_string());
            }
            _ = cast_transition.cancelled() => {
                return Err("Qobuz Connect start was superseded by a newer Cast request".to_string());
            }
            value = future => value,
        };
        if !self.enable_intent.is_current(enable_token) {
            Err("Qobuz Connect was disabled".to_string())
        } else if !cast_transition.is_current() {
            Err("Qobuz Connect start was superseded by a newer Cast request".to_string())
        } else {
            Ok(value)
        }
    }

    async fn acquire_cast_transition(
        &self,
        enable_token: QconnectEnableToken,
    ) -> Result<crate::cast_qt::CastQconnectTransitionLease, String> {
        let cast_epoch = crate::cast_qt::begin_qconnect_start_intent();
        self.acquire_exact_cast_transition(enable_token, cast_epoch)
            .await
    }

    async fn acquire_exact_cast_transition(
        &self,
        enable_token: QconnectEnableToken,
        cast_epoch: crate::cast_qt::CastTransitionEpoch,
    ) -> Result<crate::cast_qt::CastQconnectTransitionLease, String> {
        // Do not cancel this future after it enters Cast teardown: it may own
        // detached renderer handles that must reach the tracked physical fence.
        // Cast bounds/cancels its own pre-teardown waits. Enabled intent is
        // revalidated immediately after the cancellation-safe handoff returns.
        let transition = crate::cast_qt::disconnect_before_qconnect_start_exact(cast_epoch).await?;
        self.enable_intent
            .is_current(enable_token)
            .then_some(transition)
            .ok_or_else(|| "Qobuz Connect was disabled before renderer handoff".to_string())
    }

    async fn acquire_cast_restore_transition(
        &self,
        cast_epoch: crate::cast_qt::CastTransitionEpoch,
    ) -> Result<crate::cast_qt::CastQconnectTransitionLease, String> {
        crate::cast_qt::disconnect_before_qconnect_restore(cast_epoch).await
    }

    pub(crate) fn cast_restore_token_is_current(&self, token: QconnectDisabledToken) -> bool {
        self.enable_intent.is_disabled_current(token)
    }

    pub(crate) fn has_enabled_intent(&self) -> bool {
        self.enable_intent.current_token().is_some()
    }

    /// Admit the owner observation that will produce an async playback
    /// snapshot, returning its exact authority generation with the permit.
    pub fn try_owner_action_permit_observed(
        &self,
    ) -> Option<(OwnerAuthorityToken, AuthorityActionPermit)> {
        self.authority.try_owner_action_permit_observed()
    }

    /// Classify a playback observation without conflating an installed guest
    /// with the transient fence held while a candidate is still fallible.
    pub fn observe_owner_authority(&self) -> OwnerAuthorityObservation {
        self.authority.observe_owner_authority()
    }

    pub(crate) async fn playback_memory_fence(&self) -> Result<
        (tokio::sync::OwnedMutexGuard<()>, qconnect_app::OwnerActionFence), String,
    > {
        self.delegation_host.playback_memory_fence().await
    }

    /// Preserve already-stamped owner work across a fallible candidate fence.
    /// It returns `None` only once that exact owner generation is truly stale.
    pub async fn wait_for_exact_owner_action_permit(
        &self,
        token: OwnerAuthorityToken,
    ) -> Option<AuthorityActionPermit> {
        self.authority
            .wait_for_exact_owner_action_permit(token)
            .await
    }

    pub fn try_transport_action_permit(&self) -> Option<AuthorityActionPermit> {
        self.authority.try_transport_action_permit()
    }

    fn begin_runtime_action(&self) -> Result<AuthorityActionPermit, String> {
        self.begin_runtime_action_if_running()?
            .ok_or_else(|| "QConnect service is not running".to_string())
    }

    fn begin_runtime_action_if_running(&self) -> Result<Option<AuthorityActionPermit>, String> {
        let Some(stamp) = lock_inner(&self.inner)
            .runtime
            .as_ref()
            .map(|runtime| runtime.stamp)
        else {
            return Ok(None);
        };
        self.authority
            .try_runtime_action_permit(stamp)
            .map(Some)
            .ok_or_else(|| "QConnect runtime authority changed".to_string())
    }

    async fn owner_app_id(&self) -> Result<String, String> {
        let client = self
            .runtime
            .core()
            .client()
            .read()
            .await
            .clone()
            .ok_or_else(|| "qconnect-lan-owner-client-unavailable".to_string())?;
        client
            .app_id()
            .await
            .map_err(|_| "qconnect-lan-app-id-unavailable".to_string())
    }

    async fn start_lan(
        &self,
        enable_token: QconnectEnableToken,
        stamp: AuthorityStamp,
        qws_endpoint: &str,
    ) -> Result<(), String> {
        if !self.lan_lifecycle.teardown_safe() {
            return Err("qconnect-lan-physical-teardown-unsafe".to_string());
        }
        if !self.enable_intent.is_current(enable_token) {
            return Err("qconnect-lan-disabled".to_string());
        }
        if self.lan.lock().await.is_some() {
            return self
                .enable_intent
                .is_current(enable_token)
                .then_some(())
                .ok_or_else(|| "qconnect-lan-disabled".to_string());
        }
        if !self.enable_intent.is_current(enable_token) || !self.authority.is_current(stamp) {
            return Err("qconnect-lan-owner-superseded".to_string());
        }

        let endpoint_policy =
            EndpointPolicy::from_trusted_endpoints(qbz_qobuz::endpoints::BASE_URL, qws_endpoint)
                .map_err(|_| "qconnect-lan-endpoint-policy-invalid".to_string())?;
        let app_id = self
            .await_while_enabled(enable_token, self.owner_app_id())
            .await??;
        if !self.enable_intent.is_current(enable_token) {
            return Err("qconnect-lan-disabled".to_string());
        }
        let max_audio_quality =
            max_audio_quality_for_quality(crate::playback_qt::local_playback_quality().0);
        let custom_name = self.custom_device_name.read().await.clone();
        if !self.enable_intent.is_current(enable_token) {
            return Err("qconnect-lan-disabled".to_string());
        }
        let current_session_id = self
            .delegation_host
            .current_projected_session_id(stamp)
            .ok_or_else(|| "qconnect-lan-owner-session-unavailable".to_string())?;
        if !self.enable_intent.is_current(enable_token) || !self.authority.is_current(stamp) {
            return Err("qconnect-lan-owner-superseded".to_string());
        }
        let coordinator = self.coordinator.clone();
        let callback_intent = Arc::clone(&self.enable_intent);
        let callback_authority = Arc::clone(&self.authority);
        let runtime_handle = tokio::runtime::Handle::current();
        let bridge_handle = runtime_handle.clone();

        let started = self
            .lan_lifecycle
            .start(
                move || {
                    QtLanRuntime::start(
                        bridge_handle,
                        endpoint_policy,
                        app_id,
                        max_audio_quality,
                        Some(current_session_id),
                        move |candidate| {
                            let coordinator = coordinator.clone();
                            let callback_intent = Arc::clone(&callback_intent);
                            let callback_authority = Arc::clone(&callback_authority);
                            async move {
                                if !lan_callback_is_current(
                                    callback_intent.as_ref(),
                                    callback_authority.as_ref(),
                                    stamp,
                                ) {
                                    return;
                                }
                                match coordinator.admit(candidate).await {
                                    Ok(generation) => log::info!(
                                        "[QConnect LAN] handoff admitted generation={generation}"
                                    ),
                                    Err(error) => log::warn!(
                                        "[QConnect LAN] handoff admission rejected: {error}"
                                    ),
                                }
                            }
                        },
                    )
                },
                self.enable_intent.cancelled(enable_token),
            )
            .await
            .map_err(|error| error.to_string())?;

        if !self.enable_intent.is_current(enable_token) || !self.authority.is_current(stamp) {
            if let Err(error) = self.lan_lifecycle.shutdown(started).await {
                log::warn!("[QConnect LAN] stale listener teardown failed: {error}");
            }
            return Err("qconnect-lan-owner-superseded".to_string());
        }

        let projection = started.projection();
        let mut display = projection.display_info();
        display.friendly_name =
            crate::qconnect_transport_qt::resolve_qconnect_friendly_name(custom_name.as_deref());
        display.max_audio_quality = max_audio_quality;
        projection.update_display(display);
        let port = started.port();
        let mut pending = Some(started);
        let mut lan = self.lan.lock().await;
        let installed = self
            .enable_intent
            .commit_if_current(enable_token, || {
                if !self.authority.is_current(stamp) || lan.is_some() {
                    return false;
                }
                self.delegation_host.attach_projection(projection);
                *lan = pending.take();
                log::info!("[QConnect LAN] listener ready port={port:?}");
                dev_push_event("LAN receiver listening".to_string());
                true
            })
            .unwrap_or(false);
        drop(lan);
        if installed {
            return Ok(());
        }

        if let Some(stale) = pending {
            if let Err(error) = self.lan_lifecycle.shutdown(stale).await {
                log::warn!("[QConnect LAN] rejected listener teardown failed: {error}");
            }
        }
        Err("qconnect-lan-disabled-or-owner-superseded".to_string())
    }

    async fn stop_lan(&self) -> Result<(), String> {
        self.delegation_host.detach_projection();
        let runtime = self.lan.lock().await.take();
        if let Some(runtime) = runtime {
            self.lan_lifecycle
                .shutdown(runtime)
                .await
                .map_err(|error| error.to_string())?;
        }
        self.lan_lifecycle
            .settle()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn wait_for_owner_session_ready(
        &self,
        enable_token: QconnectEnableToken,
        stamp: AuthorityStamp,
    ) -> Result<(), String> {
        const OWNER_SESSION_TIMEOUT: Duration = Duration::from_secs(30);
        let wait = async {
            loop {
                if !self.enable_intent.is_current(enable_token) {
                    return Err("qconnect disabled before session acceptance".into());
                }
                if !self.authority.is_current(stamp) {
                    return Err("qconnect owner authority changed before session acceptance".into());
                }
                let lifecycle = lock_inner(&self.inner).lifecycle_state;
                match lifecycle {
                    QconnectLifecycleState::Connected => return Ok(()),
                    QconnectLifecycleState::Off | QconnectLifecycleState::Exhausted => {
                        return Err(format!(
                            "qconnect owner session failed before acceptance ({lifecycle:?})"
                        ));
                    }
                    _ => {
                        if !self.enable_intent.is_current(enable_token) {
                            return Err("qconnect disabled before session acceptance".into());
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
        };
        tokio::time::timeout(OWNER_SESSION_TIMEOUT, wait)
            .await
            .map_err(|_| "qconnect owner session acceptance timed out".to_string())?
    }

    /// Retire only the initial owner runtime installed by one connect attempt.
    /// This deliberately avoids the global disconnect path: a late failed
    /// attempt must not invalidate enabled intent or tear down a newer runtime.
    async fn retire_initial_owner_if_current(
        &self,
        enable_token: QconnectEnableToken,
        stamp: AuthorityStamp,
        error: Option<String>,
    ) {
        let mut error = error;
        let current_retirement = self.enable_intent.commit_if_current(enable_token, || {
            let mut inner = lock_inner(&self.inner);
            if inner.runtime.as_ref().map(|runtime| runtime.stamp) != Some(stamp) {
                return None;
            }
            self.delegation_host
                .projection_slot()
                .clear_if_current(&self.authority, stamp);
            inner.lifecycle_state = QconnectLifecycleState::Off;
            inner.last_pushed_queue_ids = None;
            if let Some(error) = error.take() {
                inner.last_error = Some(error);
            }
            inner.runtime.take()
        });
        let runtime = match current_retirement {
            Some(runtime) => runtime,
            None => {
                // Disabled intent may already be tearing this stamp down. Help
                // retire it exactly, but never publish its stale error.
                let mut inner = lock_inner(&self.inner);
                if inner.runtime.as_ref().map(|runtime| runtime.stamp) != Some(stamp) {
                    return;
                }
                self.delegation_host
                    .projection_slot()
                    .clear_if_current(&self.authority, stamp);
                inner.lifecycle_state = QconnectLifecycleState::Off;
                inner.last_pushed_queue_ids = None;
                inner.runtime.take()
            }
        };

        if let Some(runtime) = runtime {
            runtime.event_loop.abort();
            {
                let mut sync = runtime.sync_state.lock().await;
                sync.watchdog_generation = sync.watchdog_generation.wrapping_add(1);
                sync.session = QconnectSessionState::default();
                sync.session_renderer_states.clear();
            }
            let _ = runtime.app.disconnect().await;
            let _ = runtime.event_loop.await;
        }
    }

    fn record_connect_error_if_current(&self, enable_token: QconnectEnableToken, message: String) {
        let _ = self.enable_intent.commit_if_current(enable_token, || {
            let mut inner = lock_inner(&self.inner);
            if inner.runtime.is_none() {
                inner.lifecycle_state = QconnectLifecycleState::Off;
                inner.connecting_token = None;
                inner.last_error = Some(message);
            }
        });
    }

    pub async fn is_running(&self) -> bool {
        let inner = lock_inner(&self.inner);
        inner.runtime.is_some()
            || inner.connecting_token.is_some()
            || self.lan_lifecycle.start_pending()
            || !self.lan_lifecycle.teardown_safe()
            || self.teardown_incomplete.load(Ordering::Acquire)
    }

    /// Update the cached custom device name (Settings > Playback). The name
    /// is only announced by `bootstrap_remote_presence` during `connect()`,
    /// so — mirroring the Tauri `v2_qconnect_set_device_name` — a rename
    /// takes effect on the NEXT connect; the live session is untouched.
    /// `None` clears the override (falls back to "Qbz - {hostname}").
    /// Persistence is the caller's job
    /// (`qconnect_transport_qt::persist_device_name`).
    pub async fn set_custom_device_name(&self, name: Option<String>) {
        *self.custom_device_name.write().await = name.clone();
        let projection = self.lan.lock().await.as_ref().map(QtLanRuntime::projection);
        if let Some(projection) = projection {
            let mut display = projection.display_info();
            display.friendly_name =
                crate::qconnect_transport_qt::resolve_qconnect_friendly_name(name.as_deref());
            projection.update_display(display);
        }
    }

    /// D5 (offline-MODE): force-disconnect on every transition INTO offline
    /// (induced or real), so an established session never outlives the offline
    /// gate. Spawned once at service init (the §11.5 wiring, next to
    /// `init_service`). Idempotent: skips when no runtime is alive. Uses the
    /// SAME `disconnect()` path as the UI toggle (force-Offs lifecycle, shuts
    /// the transport down — which also kills its 60s idle-retry rearm — disarms
    /// the watchdog and clears the renderer/cast UI), then clears the bar's
    /// connected flag exactly like the manual toggle does. Deliberately NO
    /// auto-reconnect on the online edge: QConnect only reconnects through its
    /// existing user-facing flows (D5).
    pub fn spawn_offline_force_disconnect(self: &Arc<Self>, handle: &tokio::runtime::Handle) {
        let service = Arc::clone(self);
        handle.spawn(async move {
            let mut rx = crate::offline_fwd::engine().subscribe();
            let mut was_offline = rx.borrow_and_update().is_offline();
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                let is_offline = rx.borrow_and_update().is_offline();
                let entered_offline = is_offline && !was_offline;
                was_offline = is_offline;
                if !entered_offline {
                    continue;
                }
                if !service.is_running().await {
                    continue;
                }
                log::info!(
                    "[QConnect] Offline mode entered; force-disconnecting Qobuz Connect (D5)"
                );
                dev_push_event("-> force-disconnect (offline mode)".to_string());
                let mut authority_safe = false;
                for attempt in 1..=3 {
                    match service.disconnect_safely().await {
                        Ok(outcome) if outcome.authority_safe => {
                            authority_safe = true;
                            if !outcome.owner_restored {
                                log::warn!(
                                    "[QConnect] offline teardown was safe but owner playback restoration failed"
                                );
                            }
                            break;
                        }
                        Ok(_) => {}
                        Err(err) => log::warn!(
                            "[QConnect] offline force-disconnect attempt {attempt} failed: {err}"
                        ),
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                if authority_safe {
                    // Mirror the manual toggle's tail only after authority is
                    // proven quiescent; an unsafe timeout must not claim Off.
                    publish::connected(false);
                }
            }
        });
    }

    /// Report this device's playback state to the cloud while QBZ is the ACTIVE
    /// LOCAL renderer (driven by the playback poll loop). Mirrors the Tauri
    /// `v2_qconnect_report_playback_state` essentials: self-gates on
    /// is_local_renderer_active (no-op when not connected, or when a PEER owns
    /// playback), resolves the current/next queue_item_id from the playing track
    /// (the frontend doesn't track qids), sends a RndrSrvrStateUpdated, and keeps
    /// the app's renderer position in sync. `position_ms`/`duration_ms` are in
    /// MILLISECONDS (the QConnect protocol unit).
    pub async fn report_playback_state(
        &self,
        playing_state: i32,
        position_ms: i64,
        duration_ms: i64,
        track_id: u64,
        buffer_state: PlaybackBufferState,
    ) {
        let Ok(Some(_runtime_action)) = self.begin_runtime_action_if_running() else {
            return;
        };
        let (app, sync_state) = {
            let guard = lock_inner(&self.inner);
            match guard.runtime.as_ref() {
                Some(runtime) => (Arc::clone(&runtime.app), Arc::clone(&runtime.sync_state)),
                None => return,
            }
        };

        // Only report when WE are the active renderer. When a peer renderer owns
        // playback (QBZ is acting as a controller) the renderer reports come from
        // the peer, not us.
        {
            let state = sync_state.lock().await;
            if !is_local_renderer_active(&state.session) {
                return;
            }
        }

        let (mut current_qid, mut next_qid, mut queue_version) =
            resolve_queue_item_ids_by_track_id(&app, &sync_state, track_id).await;
        if current_qid.is_none() {
            // Becoming the active local renderer is not necessarily a core
            // track edge: playback may already be running when Connect joins.
            // In that case the poll's transition-only queue sync never fires,
            // and sending a position report with no matching queue item makes
            // the cloud reject us every two seconds. Reconcile first, then
            // report only once the cloud snapshot can name this track.
            self.sync_local_queue_if_changed().await;
            (current_qid, next_qid, queue_version) =
                resolve_queue_item_ids_by_track_id(&app, &sync_state, track_id).await;
            if current_qid.is_none() {
                log::debug!(
                    "[QConnect] renderer report deferred: track {track_id} is not in the cloud queue"
                );
                return;
            }
        }

        let report = build_renderer_playback_report(
            Uuid::new_v4().to_string(),
            queue_version,
            RendererPlaybackSnapshot {
                playing_state,
                buffer_state,
                position_ms: Some(position_ms),
                duration_ms: Some(duration_ms),
                current_queue_item_id: current_qid,
                next_queue_item_id: next_qid,
            },
        );
        match app.send_renderer_report_command(report).await {
            Ok(()) => {
                let mut state = sync_state.lock().await;
                confirm_local_playback_state_asserted(&mut state);
            }
            Err(err) => log::warn!("[QConnect] Failed to report playback state: {err}"),
        }

        if position_ms >= 0 {
            app.update_renderer_position(position_ms as u64).await;
        }

        // File quality comes from the decoder; device quality comes only from
        // the QBZ-owned output stream. A PCM container does not establish
        // the effective device bit depth. Reports deduplicate in qconnect-app.
        let player = self.runtime.core().player();
        let sample_rate = player.state.get_sample_rate();
        let bit_depth = player.state.get_bit_depth();
        if let Some(snapshot) =
            build_file_audio_quality_snapshot(sample_rate, bit_depth, QCONNECT_RENDERER_CHANNELS)
        {
            if let Err(err) = app
                .report_file_audio_quality_if_changed(queue_version, snapshot)
                .await
            {
                log::warn!("[QConnect] Failed to report file audio quality: {err}");
            }
            if let Some((output_rate, output_channels)) = player.state.output_stream_format() {
                if let Err(err) = app
                    .report_device_audio_quality_if_changed(
                        queue_version,
                        output_rate as i32,
                        0, // Effective device bit depth is not known from a PCM container.
                        output_channels as i32,
                    )
                    .await
                {
                    log::warn!("[QConnect] Failed to report device audio quality: {err}");
                }
            }
        }
    }

    /// Establish the QConnect session. Gated on an initialized API client (the
    /// qws/createToken discovery needs it). Idempotent while a runtime is live.
    pub async fn connect(&self) -> Result<(), String> {
        // Publish the Cast epoch BEFORE renewing enabled intent. A Cast already
        // inside its suspend seam snapshots E, revalidates its own epoch, then
        // CAS-disables E; this order makes a fresh click win in every interleave.
        let cast_epoch = crate::cast_qt::begin_qconnect_start_intent();
        let enable_token = self.enable_intent.enable_new_intent();
        let cast_transition = self
            .acquire_exact_cast_transition(enable_token, cast_epoch)
            .await?;
        self.connect_with_token_and_cast_transition(enable_token, Some(cast_transition), None)
            .await
    }

    /// Restore the QConnect session suspended by one exact Cast lifetime. The
    /// Cast epoch is validated while acquiring the already-ordered Cast →
    /// QConnect handoff lane, so a late restore cannot evict a newer renderer.
    pub(crate) async fn connect_for_cast_restore(
        &self,
        cast_epoch: crate::cast_qt::CastTransitionEpoch,
        restore_token: QconnectDisabledToken,
    ) -> Result<(), String> {
        let cast_transition = self.acquire_cast_restore_transition(cast_epoch).await?;
        let enable_token = self
            .enable_intent
            .enable_if_disabled(restore_token)
            .ok_or_else(|| {
                "Qobuz Connect restore was superseded by newer user intent".to_string()
            })?;
        self.connect_with_token_and_cast_transition(
            enable_token,
            Some(cast_transition),
            Some(restore_token),
        )
        .await
    }

    async fn connect_with_token(&self, enable_token: QconnectEnableToken) -> Result<(), String> {
        self.connect_with_token_and_cast_transition(enable_token, None, None)
            .await
    }

    async fn connect_with_token_and_cast_transition(
        &self,
        enable_token: QconnectEnableToken,
        cast_transition: Option<crate::cast_qt::CastQconnectTransitionLease>,
        restore_token: Option<QconnectDisabledToken>,
    ) -> Result<(), String> {
        if !self.enable_intent.is_current(enable_token) {
            return Err("Qobuz Connect was disabled".to_string());
        }
        // Cast and QConnect are mutually exclusive renderers. Claim the shared
        // handoff lane before any network preflight so a newer renderer intent
        // is visible immediately, not after a slow API call.
        let cast_transition = match cast_transition {
            Some(transition) => transition,
            None => self.acquire_cast_transition(enable_token).await?,
        };
        if !self.enable_intent.is_current(enable_token) {
            return Err("Qobuz Connect was disabled".to_string());
        }
        let api_initialized = self
            .await_connect_handoff(
                enable_token,
                &cast_transition,
                self.runtime.core().is_api_initialized(),
            )
            .await?;
        if !api_initialized {
            return Err("Qobuz API is not initialized; cannot start Qobuz Connect".to_string());
        }
        // D5 (offline-MODE): QConnect is not available in ANY offline mode,
        // induced or real — refuse before touching the transport. Sessions that
        // were already up when offline was entered are torn down by the
        // force-disconnect watcher (`spawn_offline_force_disconnect`).
        if crate::offline_fwd::engine().is_offline() {
            log::info!("[QConnect] connect() refused: offline mode active (D5)");
            crate::toast_qt::error(qbz_i18n::t("Qobuz Connect is unavailable while offline"));
            return Err("Qobuz Connect is unavailable while offline".to_string());
        }

        let _lifecycle = self
            .await_connect_handoff(enable_token, &cast_transition, self.lifecycle_gate.lock())
            .await?;
        if !self.enable_intent.is_current(enable_token) {
            return Err("Qobuz Connect was disabled".to_string());
        }
        if self.teardown_incomplete.load(Ordering::Acquire) {
            return Err("qconnect-authority-teardown-incomplete".to_string());
        }
        let prior_restore = self
            .await_connect_handoff(
                enable_token,
                &cast_transition,
                self.delegation_host.await_owner_playback_restore(),
            )
            .await?;
        if let Err(error) = prior_restore {
            let message = format!("qconnect owner rollback incomplete: {error}");
            self.record_connect_error_if_current(enable_token, message.clone());
            return Err(message);
        }

        // Claim the pre-runtime slot at the same synchronous boundary that
        // validates enabled intent. A repeated call for the same intent reports
        // "in progress"; a stale claim from an invalidated intent is replaceable.
        let claimed = self
            .enable_intent
            .commit_if_current(enable_token, || {
                let mut guard = lock_inner(&self.inner);
                if guard.runtime.is_some() {
                    return Ok(false);
                }
                if guard.connecting_token == Some(enable_token) {
                    return Err("QConnect connect is already in progress".to_string());
                }
                guard.connecting_token = Some(enable_token);
                guard.lifecycle_state = QconnectLifecycleState::Connecting;
                guard.last_error = None;
                Ok(true)
            })
            .ok_or_else(|| "Qobuz Connect was disabled".to_string())??;
        if !claimed {
            cast_transition.commit_qconnect_started(restore_token).await;
            return Ok(());
        }

        let config = match self
            .await_connect_handoff(
                enable_token,
                &cast_transition,
                resolve_transport_config(&self.runtime),
            )
            .await
        {
            Ok(Ok(config)) => config,
            Ok(Err(err)) => {
                self.record_connect_error_if_current(enable_token, err.clone());
                return Err(err);
            }
            Err(err) => {
                self.record_connect_error_if_current(enable_token, err.clone());
                return Err(err);
            }
        };
        if !self.enable_intent.is_current(enable_token) {
            return Err("Qobuz Connect was disabled".to_string());
        }

        let qws_endpoint = config.endpoint_url.clone();
        let stamp = self.authority.reserve(AuthorityOrigin::Owner);
        let projection = self.delegation_host.projection_slot();
        let transport = Arc::new(NativeWsTransport::new());
        let sync_state = Arc::new(Mutex::new(QconnectRemoteSyncState::default()));
        let engine = QtRendererEngine::owner(
            Arc::clone(&self.runtime),
            Arc::clone(&self.authority),
            stamp,
        );
        let sink = Arc::new(QtQconnectEventSink::new(
            engine,
            Arc::clone(&self.runtime),
            Arc::clone(&sync_state),
            Arc::clone(&self.authority),
            stamp,
            projection.clone(),
        ));
        let app = Arc::new(QconnectApp::new(
            Arc::clone(&transport) as Arc<NativeWsTransport>,
            Arc::clone(&sink),
            Arc::clone(&sync_state),
        ));
        sink.set_app(&app);

        // The transport broadcast has no replay. Subscribe before connect so
        // Connected/Auth/Subscribed/SessionEstablished cannot race past this
        // runtime while the WebSocket handshake is completing.
        let transport_rx = app.subscribe_transport_events();

        let app_connect = tokio::select! {
            biased;
            _ = self.enable_intent.cancelled(enable_token) => {
                let _ = app.disconnect().await;
                return Err("Qobuz Connect was disabled during transport connect".to_string());
            }
            _ = cast_transition.cancelled() => {
                let error =
                    "Qobuz Connect start was superseded during transport connect".to_string();
                self.record_connect_error_if_current(enable_token, error.clone());
                let _ = app.disconnect().await;
                return Err(error);
            }
            result = app.connect(config.clone()) => result,
        };
        if let Err(err) = app_connect {
            let message = format!("qconnect transport connect failed: {err}");
            self.record_connect_error_if_current(enable_token, message.clone());
            return Err(message);
        }
        if !self.enable_intent.is_current(enable_token) {
            let _ = app.disconnect().await;
            return Err("Qobuz Connect was disabled during transport connect".to_string());
        }

        let idle_retry_active = config.reconnect_idle_retry_ms > 0;
        let host: Arc<dyn SessionLoopHost> = Arc::new(QtSessionLoopHost {
            app: Arc::clone(&app),
            sync_state: Arc::clone(&sync_state),
            inner: Arc::clone(&self.inner),
            authority: Arc::clone(&self.authority),
            stamp,
            sink: Arc::clone(&sink),
            runtime: Arc::clone(&self.runtime),
            projection: projection.clone(),
        });

        let mut host = Some(host);
        let mut transport_rx = Some(transport_rx);
        if let Err(error) = cast_transition.revalidate_no_cast().await {
            self.record_connect_error_if_current(enable_token, error.clone());
            let _ = app.disconnect().await;
            return Err(error);
        }
        let installed = self
            .enable_intent
            .commit_if_current(enable_token, || {
                let mut guard = lock_inner(&self.inner);
                if guard.runtime.is_some() || guard.connecting_token != Some(enable_token) {
                    return false;
                }
                if !projection.install_authority(&self.authority, stamp, None) {
                    guard.connecting_token = None;
                    guard.lifecycle_state = QconnectLifecycleState::Off;
                    return false;
                }
                let app_for_loop = Arc::clone(&app);
                let host = host.take().expect("connect host consumed once");
                let transport_rx = transport_rx.take().expect("connect receiver consumed once");
                let event_loop = tokio::spawn(async move {
                    app_for_loop
                        .run_session_loop(host, transport_rx, idle_retry_active)
                        .await;
                });
                guard.last_error = None;
                guard.last_pushed_queue_ids = None;
                guard.connecting_token = None;
                guard.runtime = Some(QtQconnectRuntime {
                    stamp,
                    app: Arc::clone(&app),
                    config: config.clone(),
                    event_loop,
                    sync_state: Arc::clone(&sync_state),
                });
                true
            })
            .unwrap_or(false);
        let cast_receipt = cast_transition.release();
        if !installed {
            let error = "qconnect owner install was disabled or superseded".to_string();
            self.record_connect_error_if_current(enable_token, error.clone());
            let _ = app.disconnect().await;
            return Err(error);
        }

        let custom_name = self.custom_device_name.read().await.clone();
        let bootstrap = tokio::select! {
            biased;
            _ = self.enable_intent.cancelled(enable_token) => {
                let error = "Qobuz Connect was disabled during bootstrap".to_string();
                self.retire_initial_owner_if_current(enable_token, stamp, None).await;
                return Err(error);
            }
            result = bootstrap_remote_presence(&app, custom_name, &self.authority, stamp) => result,
        };
        if let Err(err) = bootstrap {
            let error = format!("qconnect bootstrap failed: {err}");
            self.retire_initial_owner_if_current(enable_token, stamp, Some(error.clone()))
                .await;
            return Err(error);
        }
        if !self.enable_intent.is_current(enable_token) || !self.authority.is_current(stamp) {
            let error = "qconnect owner authority changed during bootstrap".to_string();
            self.retire_initial_owner_if_current(enable_token, stamp, Some(error.clone()))
                .await;
            return Err(error);
        }

        // D5 race close: offline may have been entered while this connect was
        // in flight (the watcher skips while no runtime is alive). Re-check now
        // that the runtime is set; any later flip is the watcher's job.
        if crate::offline_fwd::engine().is_offline() {
            log::info!("[QConnect] offline mode entered during connect(); tearing down (D5)");
            self.enable_intent.disable();
            let _ = self.disconnect_with_owner_policy_locked(true).await;
            return Err("Qobuz Connect is unavailable while offline".to_string());
        }

        if let Err(error) = self.wait_for_owner_session_ready(enable_token, stamp).await {
            self.retire_initial_owner_if_current(enable_token, stamp, Some(error.clone()))
                .await;
            return Err(error);
        }
        let owner_ready = self
            .await_while_enabled(
                enable_token,
                self.coordinator
                    .declare_owner_ready_if(|| self.enable_intent.is_current(enable_token)),
            )
            .await
            .unwrap_or(false);
        if !owner_ready {
            if self.enable_intent.is_current(enable_token) {
                self.coordinator.shutdown().await;
            }
            let error = "qconnect delegation coordinator did not enter OwnerReady".to_string();
            self.retire_initial_owner_if_current(enable_token, stamp, Some(error.clone()))
                .await;
            return Err(error);
        }
        if !self.enable_intent.is_current(enable_token) {
            let error = "Qobuz Connect was disabled before LAN startup".to_string();
            self.retire_initial_owner_if_current(enable_token, stamp, Some(error.clone()))
                .await;
            return Err(error);
        }
        if let Err(error) = self.start_lan(enable_token, stamp, &qws_endpoint).await {
            if !self.enable_intent.is_current(enable_token) || !self.authority.is_current(stamp) {
                self.retire_initial_owner_if_current(enable_token, stamp, Some(error.clone()))
                    .await;
                return Err(error);
            }
            // LAN is a silent capability of the global QConnect service. A bind
            // failure is diagnostic and leaves the accepted owner cloud session
            // intact; it never creates a secondary user-facing mode or toggle.
            let _ = self.enable_intent.commit_if_current(enable_token, || {
                log::warn!("[QConnect LAN] listener unavailable: {error}");
                dev_push_event(format!("LAN receiver unavailable: {error}"));
            });
        }

        if !self.enable_intent.is_current(enable_token) {
            return Err("Qobuz Connect was disabled during connect".to_string());
        }
        cast_receipt.commit_qconnect_started(restore_token).await;
        Ok(())
    }

    async fn disconnect_with_owner_policy(
        &self,
        restore_owner: bool,
    ) -> Result<QconnectDisconnectOutcome, String> {
        // Cancellation must be observable by a connect that currently owns
        // the lifecycle gate. Waiting for that connect before invalidating its
        // token would turn disable into a full bootstrap/session timeout.
        let disabled_token = self.enable_intent.disable();
        {
            let mut inner = lock_inner(&self.inner);
            inner.connecting_token = None;
            if inner.runtime.is_none() {
                inner.lifecycle_state = QconnectLifecycleState::Off;
            }
        }
        let _lifecycle = self.lifecycle_gate.lock().await;
        if self.enable_intent.current_token().is_some() {
            return Err("qconnect-disable-superseded-by-enable".to_string());
        }
        let mut outcome = self
            .disconnect_with_owner_policy_locked(restore_owner)
            .await?;
        outcome.cast_restore_token = Some(disabled_token);
        Ok(outcome)
    }

    /// Cancel the exact QConnect enable currently competing with one Cast
    /// transition. Unlike a user disable, this never advances an already-off
    /// intent and never disables a newer enable that superseded the captured
    /// token. A successful `T -> E -> T2` handoff returns `T2` only while it is
    /// still the latest disabled intent after physical/authority teardown.
    pub(crate) async fn disconnect_for_cast(
        &self,
        cast_epoch: crate::cast_qt::CastTransitionEpoch,
    ) -> Result<QconnectDisconnectOutcome, String> {
        let expected_enable = self.enable_intent.current_token();
        // The manual QConnect side publishes its Cast epoch before renewing E.
        // Snapshot E first, then reject a stale Cast B before its CAS. If a
        // click lands between this check and the CAS, E changes and the CAS
        // fails; if it lands after the CAS, post-teardown enabled revalidation
        // rejects B. Together these establish a total B/C order.
        if !crate::cast_qt::qconnect_start_intent_is_current(cast_epoch) {
            return Err("qconnect-cast-suspend-superseded-by-renderer-intent".to_string());
        }
        let replacement_disabled = expected_enable.and_then(|expected| {
            let disabled = self.enable_intent.disable_if_current(expected)?;
            // Cancellation is visible before waiting for the lifecycle lane.
            // Clear only the captured claim: a newer enabled intent must keep
            // its own pre-runtime slot.
            let mut inner = lock_inner(&self.inner);
            if inner.connecting_token == Some(expected) {
                inner.connecting_token = None;
                if inner.runtime.is_none() {
                    inner.lifecycle_state = QconnectLifecycleState::Off;
                }
            }
            Some(disabled)
        });

        let _lifecycle = self.lifecycle_gate.lock().await;
        // A manual/new enable after the exact disable owns the next renderer
        // transition. Do not tear it down while it waits behind Cast's gate.
        if self.enable_intent.current_token().is_some() {
            return Err("qconnect-cast-suspend-superseded-by-enable".to_string());
        }

        let mut outcome = self.teardown_with_owner_policy_locked(true).await;
        // A later manual disable advances the disabled epoch and deliberately
        // removes Cast's restore obligation. A later enable is rejected above.
        outcome.cast_restore_token =
            replacement_disabled.filter(|token| self.enable_intent.is_disabled_current(*token));
        if self.enable_intent.current_token().is_some() {
            return Err("qconnect-cast-suspend-superseded-by-enable".to_string());
        }
        Ok(outcome)
    }

    async fn disconnect_with_owner_policy_locked(
        &self,
        restore_owner: bool,
    ) -> Result<QconnectDisconnectOutcome, String> {
        let outcome = self.teardown_with_owner_policy_locked(restore_owner).await;
        if outcome.authority_safe {
            Ok(outcome)
        } else {
            Err("qconnect-authority-teardown-incomplete".to_string())
        }
    }

    async fn teardown_with_owner_policy_locked(
        &self,
        restore_owner: bool,
    ) -> QconnectDisconnectOutcome {
        // The public entry invalidated enabled intent before waiting for this
        // lane. Do not invalidate again here: an enable requested while that
        // wait was pending is newer and must win.
        {
            let mut inner = lock_inner(&self.inner);
            inner.connecting_token = None;
            if inner.runtime.is_none() {
                inner.lifecycle_state = QconnectLifecycleState::Off;
            }
        }
        // Normative order: close HTTP/mDNS admission first, then invalidate and
        // join candidates, then stop the installed cloud authority. Both the
        // coordinator and host teardown are idempotent, including the window
        // before the initial owner reached OwnerReady.
        let lan_withdrawn = if let Err(error) = self.stop_lan().await {
            log::warn!("[QConnect] LAN teardown failed: {error}");
            false
        } else {
            !self.lan_lifecycle.start_pending() && self.lan_lifecycle.teardown_safe()
        };
        // A restore scheduled by the preceding delegated -> owner transition
        // still owns the delegation transition gate. Let that tracked task
        // reach its bounded terminal result before asking coordinator shutdown
        // to acquire the same gate; otherwise the shorter coordinator timeout
        // merely cancels its waiter and leaves a late owner runtime behind.
        let prior_owner_restore = self.delegation_host.await_owner_playback_restore().await;
        self.delegation_host
            .set_shutdown_restore_owner(restore_owner);
        let coordinator_stopped = if tokio::time::timeout(
            QCONNECT_COORDINATOR_SHUTDOWN_TIMEOUT,
            self.coordinator.shutdown(),
        )
        .await
        .is_err()
        {
            log::warn!("[QConnect] qconnect-coordinator-shutdown-timed-out");
            false
        } else {
            true
        };
        let direct_authority_shutdown_needed = !coordinator_stopped
            || self.authority.current().is_some()
            || lock_inner(&self.inner).runtime.is_some()
            || self.delegation_host.owner_snapshot_pending();
        let authority_stopped = if direct_authority_shutdown_needed {
            if tokio::time::timeout(
                QCONNECT_AUTHORITY_SHUTDOWN_TIMEOUT,
                self.delegation_host.shutdown_authority(),
            )
            .await
            .is_err()
            {
                log::warn!("[QConnect] qconnect-authority-shutdown-timed-out");
                false
            } else {
                true
            }
        } else {
            true
        };
        let shutdown_owner_restore = self.delegation_host.await_owner_playback_restore().await;
        let owner_restored = prior_owner_restore.is_ok()
            && shutdown_owner_restore.is_ok()
            && !self.delegation_host.owner_restore_pending();
        let runtime_removed = lock_inner(&self.inner).runtime.is_none();
        let authority_safe = lan_withdrawn
            && coordinator_stopped
            && authority_stopped
            && runtime_removed
            && self.authority.current().is_none();
        self.teardown_incomplete
            .store(!authority_safe, Ordering::Release);
        if authority_safe {
            *self.controller_manual.lock().await = ControllerManualBlock::default();
        }
        QconnectDisconnectOutcome {
            authority_safe,
            owner_restored,
            lan_withdrawn,
            cast_restore_token: None,
        }
    }

    pub async fn disconnect(&self) -> Result<(), String> {
        self.disconnect_safely().await.map(|_| ())
    }

    pub async fn disconnect_safely(&self) -> Result<QconnectDisconnectOutcome, String> {
        self.disconnect_with_owner_policy(true).await
    }

    /// Controller-side queue sync: when the LOCAL queue differs from the session
    /// queue (the user started a new album/playlist on QBZ while connected), push
    /// it to the session so the controller (e.g. the iOS app) sees it. Called from
    /// the playback poll loop on each track transition.
    ///
    /// Echo-safe by construction: the inbound materialize path never calls this,
    /// and we skip when the local queue already equals the cloud's last-applied
    /// queue OR the last queue we pushed. Admission normally happened before
    /// the core queue mutation. A defensive
    /// projection here also repairs restored/legacy mixed queues instead of
    /// refusing the whole cloud update.
    pub async fn sync_local_queue_if_changed(&self) {
        let Ok(Some(_runtime_action)) = self.begin_runtime_action_if_running() else {
            return;
        };
        let (app, sync_state) = {
            let guard = lock_inner(&self.inner);
            match guard.runtime.as_ref() {
                Some(runtime) => (Arc::clone(&runtime.app), Arc::clone(&runtime.sync_state)),
                None => return,
            }
        };

        // Only push while WE are the active renderer (the user is driving QBZ).
        {
            let state = sync_state.lock().await;
            if !is_local_renderer_active(&state.session) {
                return;
            }
        }

        // D8 guard: a queue built from an OFFLINE-ONLY local playlist never
        // reaches the Connect cloud. Debug level — this runs on every track
        // tick and must not spam the log (and never toasts).
        if self.runtime.core().queue_is_offline_only() {
            log::debug!("[QConnect] queue is from an offline-only playlist; skipping cloud push");
            return;
        }

        let (tracks, current_index) = self.runtime.core().get_all_queue_tracks().await;
        if tracks.is_empty() {
            return;
        }
        let source_ordered_ids: Vec<u64> = tracks.iter().map(|track| track.id).collect();
        let takeover_retry = {
            let state = sync_state.lock().await;
            local_queue_takeover_needs_retry(&state)
        };

        // Echo-suppress: skip when this is the cloud's current queue (materialized
        // inbound) so our own adoption / a remote queue change never bounces back.
        if !takeover_retry {
            let state = sync_state.lock().await;
            if let Some(applied) = &state.last_applied_queue_state {
                let applied_ids: Vec<u64> = applied
                    .queue_items
                    .iter()
                    .map(|item| item.track_id)
                    .collect();
                if applied_ids == source_ordered_ids {
                    return;
                }
            }
        }
        // ...and skip when we already pushed this exact queue (cloud echo pending).
        if !takeover_retry {
            let guard = lock_inner(&self.inner);
            if guard.last_pushed_queue_ids.as_deref() == Some(source_ordered_ids.as_slice()) {
                return;
            }
        }

        let (ordered_ids, projected_start, dropped) =
            resolvable_queue_projection(&tracks, current_index);
        if dropped > 0 {
            log::info!(
                "[QConnect] Queue projection skipped {dropped} non-Qobuz track(s) before cloud sync"
            );
            toast_unresolvable_tracks(dropped);
            dev_push_event(format!(
                "-> queue projection skipped {dropped} non-Qobuz track(s)"
            ));
        }
        if ordered_ids.is_empty() {
            lock_inner(&self.inner).last_pushed_queue_ids = Some(source_ordered_ids);
            return;
        }

        let count = ordered_ids.len();
        let start_index = projected_start.unwrap_or(0);
        let local_shuffle = self.runtime.core().get_queue_state().await.shuffle;
        let command = app
            .build_local_queue_sync_command(
                &ordered_ids,
                start_index,
                !takeover_retry && !local_shuffle,
            )
            .await;
        let append = command.command_type == QueueCommandType::CtrlSrvrQueueAddTracks;
        match app.send_queue_command(command).await {
            Ok(action_uuid) => {
                if takeover_retry {
                    let mut state = sync_state.lock().await;
                    arm_local_queue_takeover(&mut state, ordered_ids, action_uuid);
                }
                log::info!(
                    "[QConnect] Synced local queue to Connect ({count} tracks, start={start_index}, append={append})"
                );
                dev_push_event(format!(
                    "-> queue sync {count} tracks start={start_index} append={append}"
                ));
                lock_inner(&self.inner).last_pushed_queue_ids = Some(source_ordered_ids);
            }
            Err(err) => log::warn!("[QConnect] Failed to push local queue: {err}"),
        }
    }

    /// Controller play-routing: when QBZ is CONTROLLING a peer renderer and the
    /// user plays a new album/track on QBZ, route it to the peer instead of
    /// playing it locally. Returns `true` when handled remotely (caller MUST
    /// NOT play locally) and `false` when no peer is active (caller plays
    /// locally — the existing behavior runs byte-unchanged).
    ///
    /// Unlike `sync_local_queue_if_changed`, the queue push here is
    /// UNCONDITIONAL (no echo-gate, no is_local_renderer_active gate) — the user
    /// just issued a fresh play, so the current core queue IS what should run on
    /// the peer. Any legacy non-Qobuz rows are projected out while preserving
    /// order and cursor. On any send error it logs and still returns `true` (a
    /// peer owns playback; falling back to local audio would double-play).
    pub async fn play_on_peer_if_active(&self, track_id: u64) -> bool {
        let (app, sync_state) = {
            let guard = lock_inner(&self.inner);
            let Some(runtime) = guard.runtime.as_ref() else {
                return false;
            };
            (Arc::clone(&runtime.app), Arc::clone(&runtime.sync_state))
        };
        let peer_active = {
            let state = sync_state.lock().await;
            is_peer_renderer_active(&state.session)
        };
        if !peer_active {
            return false;
        }

        // D8 guard: an offline-only-playlist queue is never pushed to the
        // cloud. Returning false lets the play proceed LOCALLY; playback of
        // these tracks is fine, the prohibition is only on the cloud push.
        if self.runtime.core().queue_is_offline_only() {
            log::info!(
                "[QConnect] queue is from an offline-only playlist; not routing to peer renderer"
            );
            return false;
        }

        // (a) Push the CURRENT core queue to the peer (unconditional).
        let (tracks, current_index) = self.runtime.core().get_all_queue_tracks().await;
        if tracks.is_empty() {
            return false;
        }
        let source_ordered_ids: Vec<u64> = tracks.iter().map(|track| track.id).collect();
        let (ordered_ids, projected_start, dropped) =
            resolvable_queue_projection(&tracks, current_index);
        if dropped > 0 {
            log::info!("[QConnect] play_on_peer projected out {dropped} non-Qobuz track(s)");
            toast_unresolvable_tracks(dropped);
            dev_push_event(format!(
                "-> play_on_peer skipped {dropped} non-Qobuz track(s)"
            ));
        }
        if ordered_ids.is_empty() || !ordered_ids.contains(&track_id) {
            lock_inner(&self.inner).last_pushed_queue_ids = Some(source_ordered_ids);
            return true;
        }

        let count = ordered_ids.len();
        let track_ids: Vec<i64> = ordered_ids.iter().map(|id| *id as i64).collect();
        let start_index = projected_start.unwrap_or(0);
        let payload = json!({
            "track_ids": track_ids,
            "queue_position": start_index,
            "shuffle_mode": false,
            "shuffle_pivot_index": start_index,
            "context_uuid": Uuid::new_v4().to_string(),
            "autoplay_reset": true,
            "autoplay_loading": false,
        });
        let command = app
            .build_queue_command(QueueCommandType::CtrlSrvrQueueLoadTracks, payload)
            .await;
        match app.send_queue_command(command).await {
            Ok(_) => {
                log::info!(
                    "[QConnect] play_on_peer: pushed queue ({count} tracks, start={start_index})"
                );
                dev_push_event(format!(
                    "-> play_on_peer QueueLoadTracks {count} start={start_index}"
                ));
                lock_inner(&self.inner).last_pushed_queue_ids = Some(source_ordered_ids);
            }
            Err(err) => {
                log::warn!("[QConnect] play_on_peer: queue push failed: {err}");
                crate::toast_qt::error(qbz_i18n::t(
                    "Failed to send playback to the selected device",
                ));
                // Still handled: a peer owns playback; never fall back to local.
                return true;
            }
        }

        // (b) SetPlayerState the peer to the requested track (polls the peer
        // queue until the track appears). Errors are logged, never fall back.
        match self.play_remote_renderer_track_if_active(track_id).await {
            Ok(_) => {}
            Err(err) => {
                log::warn!("[QConnect] play_on_peer: play_remote_track failed: {err}");
                crate::toast_qt::error(qbz_i18n::t(
                    "Failed to send playback to the selected device",
                ));
            }
        }
        true
    }

    /// Controller play-next routing: when QBZ is CONTROLLING a peer renderer and
    /// the user does "Play next" on QBZ, route the track to the peer's queue
    /// (insert right after the peer's CURRENT track) instead of mutating only the
    /// LOCAL queue (which the peer never sees). Returns `true` when handled (the
    /// caller MUST NOT enqueue locally) and `false` when no peer is active (the
    /// caller does the existing local insert, byte-unchanged).
    ///
    /// Mirrors the webplayer `queue_insert_tracks` path: a single command with a
    /// fresh `context_uuid`, `autoplay_reset: false`, `autoplay_loading: false`,
    /// and `insert_after` = the renderer's current queue_item_id (omitted when
    /// unknown). Admission is the SAME single-track rule as the queue sync: a
    /// `local` / `plex` track is refused (a renderer can't play a local/Plex id;
    /// offline `qobuz_download` IS eligible). On refusal it toasts + returns
    /// `true` (handled — do NOT add it locally while controlling). The cloud
    /// echoes a `QueueUpdated` that `materialize_remote_queue` applies to the
    /// local queue, so we never mutate the local queue here (avoids divergence).
    pub async fn play_next_on_peer_if_active(&self, track_id: u64, source: Option<&str>) -> bool {
        let peer_active = self.is_peer_renderer_active().await;
        if !peer_active {
            return false;
        }

        if !self.is_track_castable(track_id, source) {
            log::info!(
                "[QConnect] play_next_on_peer: track {track_id} not Qobuz-castable; refusing"
            );
            toast_unresolvable_tracks(1);
            dev_push_event(format!("-> play_next REFUSED (non-Qobuz track {track_id})"));
            // Handled: do NOT add a non-castable track to the local queue while a
            // peer owns playback.
            return true;
        }

        // Resolve insert_after from the peer's current track (omit when unknown).
        let insert_after = self
            .effective_remote_renderer_snapshot()
            .await
            .ok()
            .flatten()
            .and_then(|(renderer, _queue, _session)| {
                renderer
                    .current_track
                    .as_ref()
                    .and_then(|item| i64::try_from(item.queue_item_id).ok())
            });

        let mut payload = json!({
            "track_ids": [track_id as i64],
            "context_uuid": Uuid::new_v4().to_string(),
            "autoplay_reset": false,
            "autoplay_loading": false,
        });
        if let Some(insert_after) = insert_after {
            payload["insert_after"] = json!(insert_after);
        }

        match self
            .send_command(QueueCommandType::CtrlSrvrQueueInsertTracks, payload)
            .await
        {
            Ok(_) => {
                log::info!(
                    "[QConnect] play_next_on_peer: inserted track {track_id} (after={insert_after:?})"
                );
                dev_push_event(format!(
                    "-> play_next QueueInsertTracks {track_id} after={insert_after:?}"
                ));
                self.note_controller_insert(insert_after, &[track_id]).await;
            }
            Err(err) => {
                log::warn!("[QConnect] play_next_on_peer: insert failed: {err}");
                // Still handled: a peer owns playback; never fall back to local.
            }
        }
        true
    }

    /// Controller add-to-queue routing: when QBZ is CONTROLLING a peer renderer
    /// and the user does "Add to queue" on QBZ, append the track to the peer's
    /// queue instead of mutating only the LOCAL queue. Returns `true` when handled
    /// and `false` when no peer is active (caller does the existing local append).
    ///
    /// Mirrors the webplayer `queue_add_tracks` path: a single append command with
    /// a fresh `context_uuid`, `autoplay_reset: false`, `autoplay_loading: false`.
    /// Same admission + echo handling as `play_next_on_peer_if_active`.
    pub async fn add_to_queue_on_peer_if_active(
        &self,
        track_id: u64,
        source: Option<&str>,
    ) -> bool {
        let peer_active = self.is_peer_renderer_active().await;
        if !peer_active {
            return false;
        }

        if !self.is_track_castable(track_id, source) {
            log::info!(
                "[QConnect] add_to_queue_on_peer: track {track_id} not Qobuz-castable; refusing"
            );
            toast_unresolvable_tracks(1);
            dev_push_event(format!(
                "-> add_to_queue REFUSED (non-Qobuz track {track_id})"
            ));
            return true;
        }

        let payload = json!({
            "track_ids": [track_id as i64],
            "context_uuid": Uuid::new_v4().to_string(),
            "autoplay_reset": false,
            "autoplay_loading": false,
        });

        match self
            .send_command(QueueCommandType::CtrlSrvrQueueAddTracks, payload)
            .await
        {
            Ok(_) => {
                log::info!("[QConnect] add_to_queue_on_peer: appended track {track_id}");
                dev_push_event(format!("-> add_to_queue QueueAddTracks {track_id}"));
            }
            Err(err) => {
                log::warn!("[QConnect] add_to_queue_on_peer: append failed: {err}");
            }
        }
        true
    }

    /// Controller add-to-queue routing for a MULTI-track batch (album / playlist /
    /// favorites bulk). Same contract as the single-track
    /// `add_to_queue_on_peer_if_active`. Admission is partial: non-Qobuz rows
    /// are counted once and omitted while every resolvable row keeps its order. A single
    /// `CtrlSrvrQueueAddTracks` carries every id (the protocol `track_ids` is a
    /// full `Vec`), and the cloud echoes a `QueueUpdated` that
    /// `materialize_remote_queue` applies locally, so we never mutate the local
    /// queue here. Returns `false` only when no peer is active (caller appends
    /// locally) or the batch is empty (caller no-ops).
    pub async fn add_to_queue_batch_on_peer_if_active(
        &self,
        tracks: &[(u64, Option<String>)],
    ) -> bool {
        if !self.is_peer_renderer_active().await {
            return false;
        }
        if tracks.is_empty() {
            return false;
        }

        let (ids_u64, dropped) = resolvable_track_ids(tracks);
        if dropped > 0 {
            log::info!(
                "[QConnect] add_to_queue_batch_on_peer: skipped {dropped} non-Qobuz track(s)"
            );
            toast_unresolvable_tracks(dropped);
            dev_push_event(format!(
                "-> add_to_queue skipped {dropped} non-Qobuz track(s) from batch of {}",
                tracks.len(),
            ));
        }
        if ids_u64.is_empty() {
            return true;
        }

        let ids: Vec<i64> = ids_u64.iter().map(|id| *id as i64).collect();
        let count = ids.len();
        let payload = json!({
            "track_ids": ids,
            "context_uuid": Uuid::new_v4().to_string(),
            "autoplay_reset": false,
            "autoplay_loading": false,
        });

        match self
            .send_command(QueueCommandType::CtrlSrvrQueueAddTracks, payload)
            .await
        {
            Ok(_) => {
                log::info!("[QConnect] add_to_queue_batch_on_peer: appended {count} tracks");
                dev_push_event(format!("-> add_to_queue QueueAddTracks {count} tracks"));
            }
            Err(err) => {
                log::warn!("[QConnect] add_to_queue_batch_on_peer: append failed: {err}");
            }
        }
        true
    }

    /// Controller play-next routing for a MULTI-track batch. Same partial
    /// admission as `add_to_queue_batch_on_peer_if_active`. The server
    /// `CtrlSrvrQueueInsertTracks` inserts the whole `track_ids` block right after
    /// `insert_after` and PRESERVES the list order, so the ids are passed in
    /// NATURAL order here (unlike the LOCAL fall-through, which reverses per-track
    /// `add_track_next` inserts to achieve the same effect).
    pub async fn play_next_batch_on_peer_if_active(
        &self,
        tracks: &[(u64, Option<String>)],
    ) -> bool {
        if !self.is_peer_renderer_active().await {
            return false;
        }
        if tracks.is_empty() {
            return false;
        }

        let (ids_u64, dropped) = resolvable_track_ids(tracks);
        if dropped > 0 {
            log::info!("[QConnect] play_next_batch_on_peer: skipped {dropped} non-Qobuz track(s)");
            toast_unresolvable_tracks(dropped);
            dev_push_event(format!(
                "-> play_next skipped {dropped} non-Qobuz track(s) from batch of {}",
                tracks.len(),
            ));
        }
        if ids_u64.is_empty() {
            return true;
        }

        // Resolve insert_after from the peer's current track (omit when unknown).
        let insert_after = self
            .effective_remote_renderer_snapshot()
            .await
            .ok()
            .flatten()
            .and_then(|(renderer, _queue, _session)| {
                renderer
                    .current_track
                    .as_ref()
                    .and_then(|item| i64::try_from(item.queue_item_id).ok())
            });

        let ids: Vec<i64> = ids_u64.iter().map(|id| *id as i64).collect();
        let count = ids.len();
        let mut payload = json!({
            "track_ids": ids,
            "context_uuid": Uuid::new_v4().to_string(),
            "autoplay_reset": false,
            "autoplay_loading": false,
        });
        if let Some(insert_after) = insert_after {
            payload["insert_after"] = json!(insert_after);
        }

        match self
            .send_command(QueueCommandType::CtrlSrvrQueueInsertTracks, payload)
            .await
        {
            Ok(_) => {
                log::info!(
                    "[QConnect] play_next_batch_on_peer: inserted {count} tracks (after={insert_after:?})"
                );
                dev_push_event(format!(
                    "-> play_next QueueInsertTracks {count} tracks after={insert_after:?}"
                ));
                self.note_controller_insert(insert_after, &ids_u64).await;
            }
            Err(err) => {
                log::warn!("[QConnect] play_next_batch_on_peer: insert failed: {err}");
            }
        }
        true
    }

    /// Resolve the `insert_after` anchor for a controller-mode PLAY-LATER: the
    /// manual-block tail while it is still ahead of the peer's current track,
    /// else the current track itself (degrades to play-next). Reconciles the
    /// `ControllerManualBlock` against the freshest snapshot first. `None` when
    /// no snapshot is available (the command then omits `insert_after`, same as
    /// the play-next path).
    async fn controller_later_anchor(&self) -> Option<i64> {
        let (renderer, queue, _session) = self
            .effective_remote_renderer_snapshot()
            .await
            .ok()
            .flatten()?;
        let mut guard = self.controller_manual.lock().await;
        guard.reconcile(&renderer, &queue);
        guard
            .later_anchor(&renderer, &queue)
            .and_then(|qid| i64::try_from(qid).ok())
    }

    /// Record a just-sent controller-mode insert (play-next or play-later) so
    /// the next `controller_later_anchor` can confirm its echo and keep the
    /// manual-block tail past it.
    async fn note_controller_insert(&self, anchor: Option<i64>, ids: &[u64]) {
        let anchor = anchor.and_then(|v| u64::try_from(v).ok());
        let Ok(Some((renderer, queue, _session))) = self.effective_remote_renderer_snapshot().await
        else {
            return;
        };
        let mut guard = self.controller_manual.lock().await;
        guard.note_sent(&renderer, &queue, anchor, ids.to_vec());
    }

    /// Controller play-LATER routing (#442): when QBZ is CONTROLLING a peer
    /// renderer, insert the track at the END of the peer's manual block (after
    /// every play-next / play-later already routed, before the source resumes)
    /// instead of right after the current track. The protocol has no "later" —
    /// the position is steered via `insert_after` against the tracked block
    /// tail (`ControllerManualBlock`); best-effort, the cloud stays the source
    /// of truth. Same admission + echo contract as `play_next_on_peer_if_active`.
    pub async fn play_later_on_peer_if_active(&self, track_id: u64, source: Option<&str>) -> bool {
        let peer_active = self.is_peer_renderer_active().await;
        if !peer_active {
            return false;
        }

        if !self.is_track_castable(track_id, source) {
            log::info!(
                "[QConnect] play_later_on_peer: track {track_id} not Qobuz-castable; refusing"
            );
            toast_unresolvable_tracks(1);
            dev_push_event(format!(
                "-> play_later REFUSED (non-Qobuz track {track_id})"
            ));
            return true;
        }

        let insert_after = self.controller_later_anchor().await;

        let mut payload = json!({
            "track_ids": [track_id as i64],
            "context_uuid": Uuid::new_v4().to_string(),
            "autoplay_reset": false,
            "autoplay_loading": false,
        });
        if let Some(insert_after) = insert_after {
            payload["insert_after"] = json!(insert_after);
        }

        match self
            .send_command(QueueCommandType::CtrlSrvrQueueInsertTracks, payload)
            .await
        {
            Ok(_) => {
                log::info!(
                    "[QConnect] play_later_on_peer: inserted track {track_id} (after={insert_after:?})"
                );
                dev_push_event(format!(
                    "-> play_later QueueInsertTracks {track_id} after={insert_after:?}"
                ));
                self.note_controller_insert(insert_after, &[track_id]).await;
            }
            Err(err) => {
                log::warn!("[QConnect] play_later_on_peer: insert failed: {err}");
                // Still handled: a peer owns playback; never fall back to local.
            }
        }
        true
    }

    /// Controller DROP-AT-POSITION routing: the queue drag-and-drop insert.
    ///
    /// Written 2026-08-10 after the owner pushed back on "the peer protocol
    /// has no insert-at-index, so the position cannot be honoured". That was
    /// wrong twice over. `CtrlSrvrQueueInsertTracks` carries `insert_after`,
    /// which is precisely an insert-at-position — it is the same field
    /// play-next and play-later already steer with, as the comment on
    /// `play_later_on_peer_if_active` says out loud ("The protocol has no
    /// 'later' — the position is steered via `insert_after`"). And even
    /// without it the local queue has no insert-at-index either, and that was
    /// solved by composing; the peer has `CtrlSrvrQueueReorderTracks` for the
    /// same composition. There was never a reason to append and shrug.
    ///
    /// `slot` is an index into the VISIBLE upcoming list — the same space
    /// `reorder_upcoming_if_remote` clamps into, and the same one the panel's
    /// `slotFromPointer` produces — so the anchor is simply the row BEFORE it:
    ///   slot 0 -> the current track (land first in upcoming)
    ///   slot k -> `upcoming_qids[k - 1]`
    ///   past the end -> the last upcoming row (append)
    ///
    /// Same admission + echo contract as its two siblings: peer must be the
    /// active renderer, the track must be Qobuz-castable, and a sent insert is
    /// recorded so the manual-block tail stays honest.
    pub async fn insert_at_slot_on_peer_if_active(
        &self,
        track_id: u64,
        source: Option<&str>,
        slot: usize,
    ) -> bool {
        self.insert_at_slot_on_peer_outcome(track_id, source, slot)
            .await
            != PeerInsertAtSlotOutcome::Inactive
    }

    pub(crate) async fn insert_at_slot_on_peer_outcome(
        &self,
        track_id: u64,
        source: Option<&str>,
        slot: usize,
    ) -> PeerInsertAtSlotOutcome {
        if !self.is_peer_renderer_active().await {
            return PeerInsertAtSlotOutcome::Inactive;
        }
        if !self.is_track_castable(track_id, source) {
            log::info!("[QConnect] insert_at_slot: track {track_id} not Qobuz-castable; refusing");
            toast_unresolvable_tracks(1);
            dev_push_event(format!(
                "-> insert_at_slot REFUSED (non-Qobuz track {track_id})"
            ));
            return PeerInsertAtSlotOutcome::HandledFailure;
        }

        let insert_after = self
            .effective_remote_renderer_snapshot()
            .await
            .ok()
            .flatten()
            .and_then(|(renderer, queue, _session)| {
                let projection = build_visible_upcoming_projection(&queue, &renderer);
                let qid = if slot == 0 {
                    projection.current_track_qid
                } else {
                    // Past the end clamps to the last row, which is the
                    // append anchor.
                    let idx = (slot - 1).min(projection.upcoming_qids.len().saturating_sub(1));
                    projection.upcoming_qids.get(idx).copied()
                };
                qid.and_then(|q| i64::try_from(q).ok())
            });

        let mut payload = json!({
            "track_ids": [track_id as i64],
            "context_uuid": Uuid::new_v4().to_string(),
            "autoplay_reset": false,
            "autoplay_loading": false,
        });
        if let Some(insert_after) = insert_after {
            payload["insert_after"] = json!(insert_after);
        }

        match self
            .send_command(QueueCommandType::CtrlSrvrQueueInsertTracks, payload)
            .await
        {
            Ok(_) => {
                log::info!(
                    "[QConnect] insert_at_slot: inserted track {track_id} at slot {slot} (after={insert_after:?})"
                );
                dev_push_event(format!(
                    "-> insert_at_slot QueueInsertTracks {track_id} slot={slot} after={insert_after:?}"
                ));
                self.note_controller_insert(insert_after, &[track_id]).await;
                PeerInsertAtSlotOutcome::Inserted
            }
            Err(err) => {
                log::warn!("[QConnect] insert_at_slot: insert failed: {err}");
                // Still handled: a peer owns playback; never fall back to local.
                PeerInsertAtSlotOutcome::HandledFailure
            }
        }
    }

    /// Controller play-LATER routing for a MULTI-track batch (#442). Same
    /// partial admission as `play_next_batch_on_peer_if_active`; the surviving
    /// block lands after the manual-block tail in NATURAL order (the
    /// server preserves list order), mirroring the local
    /// `enqueue_queue_tracks_later` semantics.
    pub async fn play_later_batch_on_peer_if_active(
        &self,
        tracks: &[(u64, Option<String>)],
    ) -> bool {
        if !self.is_peer_renderer_active().await {
            return false;
        }
        if tracks.is_empty() {
            return false;
        }

        let (ids_u64, dropped) = resolvable_track_ids(tracks);
        if dropped > 0 {
            log::info!("[QConnect] play_later_batch_on_peer: skipped {dropped} non-Qobuz track(s)");
            toast_unresolvable_tracks(dropped);
            dev_push_event(format!(
                "-> play_later skipped {dropped} non-Qobuz track(s) from batch of {}",
                tracks.len(),
            ));
        }
        if ids_u64.is_empty() {
            return true;
        }

        let insert_after = self.controller_later_anchor().await;

        let ids: Vec<i64> = ids_u64.iter().map(|id| *id as i64).collect();
        let count = ids.len();
        let mut payload = json!({
            "track_ids": ids,
            "context_uuid": Uuid::new_v4().to_string(),
            "autoplay_reset": false,
            "autoplay_loading": false,
        });
        if let Some(insert_after) = insert_after {
            payload["insert_after"] = json!(insert_after);
        }

        match self
            .send_command(QueueCommandType::CtrlSrvrQueueInsertTracks, payload)
            .await
        {
            Ok(_) => {
                log::info!(
                    "[QConnect] play_later_batch_on_peer: inserted {count} tracks (after={insert_after:?})"
                );
                dev_push_event(format!(
                    "-> play_later QueueInsertTracks {count} tracks after={insert_after:?}"
                ));
                self.note_controller_insert(insert_after, &ids_u64).await;
            }
            Err(err) => {
                log::warn!("[QConnect] play_later_batch_on_peer: insert failed: {err}");
            }
        }
        true
    }

    /// True when a PEER renderer currently owns playback (controller mode). Reads
    /// the session under the sync-state lock. Shared by the play-next /
    /// add-to-queue routing entry points.
    pub(crate) async fn is_peer_renderer_active(&self) -> bool {
        let sync_state = {
            let guard = lock_inner(&self.inner);
            let Some(runtime) = guard.runtime.as_ref() else {
                return false;
            };
            Arc::clone(&runtime.sync_state)
        };
        let state = sync_state.lock().await;
        is_peer_renderer_active(&state.session)
    }

    /// Single-track form of the shared source-aware admission contract.
    fn is_track_castable(&self, track_id: u64, source: Option<&str>) -> bool {
        qconnect_app::qconnect_source_is_resolvable(track_id, source)
    }

    // -----------------------------------------------------------------------
    // CONTROLLER-mode transport routing (`*_if_remote`). Mirror of the Tauri
    // `src-tauri/src/qconnect/service.rs` adapter. Return contract:
    // `Ok(true)` = handled remotely (do NOT run local), `Ok(false)` = fall back
    // to the local path. The load-bearing safety property: every method begins
    // with `effective_remote_renderer_snapshot()`, which returns `Some` ONLY
    // when a PEER renderer is active (both active+local ids Some AND differ).
    // In every non-controller situation it returns `Ok(false)` and the existing
    // local path runs verbatim. Diagnostics are emitted via log + dev_push_event.
    // -----------------------------------------------------------------------

    /// Send a controller command to the cloud. Mirrors the Tauri
    /// `QconnectServiceState::send_command`, including the pending-transport
    /// clear for superseded `CtrlSrvrSetPlayerState` actions.
    async fn send_command(
        &self,
        command_type: QueueCommandType,
        payload: Value,
    ) -> Result<String, String> {
        let _runtime_action = self.begin_runtime_action()?;
        let app = {
            let guard = lock_inner(&self.inner);
            guard
                .runtime
                .as_ref()
                .map(|runtime| Arc::clone(&runtime.app))
                .ok_or_else(|| "QConnect service is not running".to_string())?
        };

        if matches!(command_type, QueueCommandType::CtrlSrvrSetPlayerState) {
            let state_handle = app.state_handle();
            let mut state = state_handle.lock().await;
            let should_clear_transport_pending = state
                .pending
                .current()
                .map(|pending| pending.is_transport_control_action)
                .unwrap_or(false);
            if should_clear_transport_pending {
                log::info!(
                    "[QConnect] Clearing superseded pending transport control before sending next SET_PLAYER_STATE"
                );
                state.pending.clear();
            }
        }

        if matches!(command_type, QueueCommandType::CtrlSrvrSetVolume) {
            // A rapid volume drag fires SetVolume faster than the cloud echoes
            // SrvrCtrlVolumeChanged. Supersede the in-flight volume command
            // (latest-wins) so a drag never spams "pending queue action already
            // active". Mirrors the SetPlayerState supersede above.
            let state_handle = app.state_handle();
            let mut state = state_handle.lock().await;
            let should_clear_volume_pending = state
                .pending
                .current()
                .map(|pending| pending.is_set_volume_action)
                .unwrap_or(false);
            if should_clear_volume_pending {
                log::info!(
                    "[QConnect] Clearing superseded pending volume before sending next SET_VOLUME"
                );
                state.pending.clear();
            }
        }

        // Payloads may contain session/context identifiers. Diagnostics keep
        // only the allowlisted command kind; individual routing methods already
        // report safe scalar intent such as pause/seek/track count.
        if matches!(command_type, QueueCommandType::CtrlSrvrSetPlayerState) {
            log::debug!("[QConnect] outbound SetPlayerState");
            dev_push_event("-> SetPlayerState".to_string());
        }

        let command = app.build_queue_command(command_type, payload).await;
        app.send_queue_command(command)
            .await
            .map_err(|err| format!("qconnect send command failed: {err}"))
    }

    /// Best-effort local cursor alignment after a remote handoff so a later
    /// local takeover ("Play here") continues at the right track.
    /// `sync_current_to_id` only moves the queue pointer; it never starts
    /// audible playback. Never fails the handoff.
    async fn align_local_cursor(&self, track_id: u64) {
        if self
            .runtime
            .core()
            .sync_current_to_id(track_id)
            .await
            .is_none()
        {
            log::warn!(
                "[QConnect] cursor align: track {track_id} not found in local queue (best-effort)"
            );
        }
    }

    /// Project the active renderer from its own state. Only the local renderer
    /// may inherit the local command snapshot; peers use their session cache.
    /// Returns `None` when not connected or no renderer is active.
    pub(crate) async fn effective_active_renderer_snapshot(
        &self,
    ) -> Result<
        Option<(
            QConnectRendererState,
            QConnectQueueState,
            QconnectSessionState,
        )>,
        String,
    > {
        let (app, sync_state) = {
            let guard = lock_inner(&self.inner);
            let Some(runtime) = guard.runtime.as_ref() else {
                return Ok(None);
            };
            (Arc::clone(&runtime.app), Arc::clone(&runtime.sync_state))
        };

        let queue = app.queue_state_snapshot().await;
        let base_renderer = app.renderer_state_snapshot().await;
        let state = sync_state.lock().await;
        let session = state.session.clone();
        let Some(_) = session.active_renderer_id else {
            return Ok(None);
        };

        let renderer = active_renderer_projection(&queue, &base_renderer, &state);

        Ok(Some((renderer, queue, session)))
    }

    /// Like `effective_active_renderer_snapshot` but gated: returns `Some` ONLY
    /// when a PEER renderer is active (controller mode). Mirrors the Tauri
    /// `effective_remote_renderer_snapshot`. This is the gate for all
    /// `*_if_remote` methods.
    pub(crate) async fn effective_remote_renderer_snapshot(
        &self,
    ) -> Result<
        Option<(
            QConnectRendererState,
            QConnectQueueState,
            QconnectSessionState,
        )>,
        String,
    > {
        let Some((renderer, queue, session)) = self.effective_active_renderer_snapshot().await?
        else {
            return Ok(None);
        };

        if !is_peer_renderer_active(&session) {
            return Ok(None);
        }

        Ok(Some((renderer, queue, session)))
    }

    /// True when a PEER renderer currently owns playback (controller mode);
    /// false when not connected or when this device is the active renderer.
    /// Used by the audio-settings force-100 path to SKIP forcing local volume
    /// to 100% while controlling a peer (the bit-perfect lock is lifted then).
    pub async fn is_peer_active(&self) -> bool {
        let sync_state = {
            let guard = lock_inner(&self.inner);
            let Some(runtime) = guard.runtime.as_ref() else {
                return false;
            };
            Arc::clone(&runtime.sync_state)
        };
        let state = sync_state.lock().await;
        is_peer_renderer_active(&state.session)
    }

    /// Reduced peer-renderer playback snapshot for the now-playing seek bar.
    /// Returns `Some` ONLY when a PEER renderer is active (controller mode); the
    /// poll loop then drives the bar from the peer and skips its local body. When
    /// `None`, the poll loop falls through to the local player path verbatim.
    /// Sources position / updated_at / playing from the effective remote renderer
    /// snapshot (`playing_state == PLAYING`). Title/artist/art come from the
    /// materialized local core queue, so only these three fields are needed here.
    pub async fn remote_now_playing(&self) -> Option<RemoteNowPlaying> {
        let (renderer, queue, _session) = self
            .effective_remote_renderer_snapshot()
            .await
            .ok()
            .flatten()?;
        Some(RemoteNowPlaying {
            position_ms: renderer.current_position_ms.unwrap_or(0),
            updated_at_ms: renderer.updated_at_ms,
            playing: renderer.playing_state == Some(PLAYING_STATE_PLAYING),
            volume: renderer.volume,
            muted: renderer.muted.unwrap_or(false),
            track_id: renderer
                .current_track
                .as_ref()
                .map(|item| item.track_id)
                .unwrap_or(0),
            // The shuffle BUTTON reflects the cloud-authoritative QUEUE shuffle
            // flag, NOT the per-renderer `renderer.shuffle_mode` — the cloud never
            // populates the per-renderer shuffle field for a peer (it stays None),
            // so reading it lit the button only by luck (worked for one peer type,
            // not the other). `QConnectQueueState.shuffle_mode` is always present.
            // Matches Tauri (queueStore `isShuffle = queueState.shuffle`).
            shuffle_mode: queue.shuffle_mode,
            // QConnect wire loop_mode (1=off, 3=all, 2=one) -> UI repeat-mode
            // (0=off, 1=all, 2=one). Unknown / off -> 0.
            repeat_mode: match renderer.loop_mode {
                Some(3) => 1,
                Some(2) => 2,
                _ => 0,
            },
        })
    }

    /// Optimistically apply a queue_item_id / playing_state / position to the
    /// active peer renderer's cached state, so the UI doesn't bounce back before
    /// the cloud echo. Mirrors the Tauri `prime_remote_renderer_state`.
    async fn prime_remote_renderer_state(
        &self,
        queue_item_id: u64,
        playing_state: Option<i32>,
        current_position_ms: Option<u64>,
    ) {
        let sync_state = {
            let guard = lock_inner(&self.inner);
            let Some(runtime) = guard.runtime.as_ref() else {
                return;
            };
            Arc::clone(&runtime.sync_state)
        };

        let mut sync_state = sync_state.lock().await;
        let Some(active_renderer_id) = sync_state.session.active_renderer_id else {
            return;
        };
        if sync_state.session.local_renderer_id == Some(active_renderer_id) {
            return;
        }

        let renderer_state = ensure_session_renderer_state(&mut sync_state, active_renderer_id);
        renderer_state.current_queue_item_id = Some(queue_item_id);
        if let Some(playing_state) = playing_state {
            renderer_state.playing_state = Some(playing_state);
        }
        if let Some(current_position_ms) = current_position_ms {
            renderer_state.current_position_ms = Some(current_position_ms);
        }
        renderer_state.updated_at_ms = qconnect_now_ms();
    }

    /// Optimistically apply only a playing_state to the active peer renderer.
    /// Mirrors the Tauri `prime_remote_renderer_playing_state`.
    async fn prime_remote_renderer_playing_state(&self, playing_state: i32) {
        let sync_state = {
            let guard = lock_inner(&self.inner);
            let Some(runtime) = guard.runtime.as_ref() else {
                return;
            };
            Arc::clone(&runtime.sync_state)
        };

        let mut sync_state = sync_state.lock().await;
        let Some(active_renderer_id) = sync_state.session.active_renderer_id else {
            return;
        };
        if sync_state.session.local_renderer_id == Some(active_renderer_id) {
            return;
        }

        let renderer_state = ensure_session_renderer_state(&mut sync_state, active_renderer_id);
        renderer_state.playing_state = Some(playing_state);
        renderer_state.updated_at_ms = qconnect_now_ms();
    }

    pub async fn skip_next_if_remote(&self) -> Result<bool, String> {
        self.skip_remote_renderer_if_active(QconnectRemoteSkipDirection::Next)
            .await
    }

    pub async fn skip_previous_if_remote(&self) -> Result<bool, String> {
        self.skip_remote_renderer_if_active(QconnectRemoteSkipDirection::Previous)
            .await
    }

    /// Skip the active PEER renderer next/previous. Mirrors the Tauri
    /// `skip_remote_renderer_if_active`.
    async fn skip_remote_renderer_if_active(
        &self,
        direction: QconnectRemoteSkipDirection,
    ) -> Result<bool, String> {
        let Some(_runtime_action) = self.begin_runtime_action_if_running()? else {
            return Ok(false);
        };
        let direction_label = match direction {
            QconnectRemoteSkipDirection::Next => "next",
            QconnectRemoteSkipDirection::Previous => "previous",
        };

        let remote_context = self.effective_remote_renderer_snapshot().await?;
        let Some((renderer, queue, session)) = remote_context else {
            let reason = {
                let sync_state = {
                    let guard = lock_inner(&self.inner);
                    let Some(runtime) = guard.runtime.as_ref() else {
                        return Ok(false);
                    };
                    Arc::clone(&runtime.sync_state)
                };
                let session = sync_state.lock().await.session.clone();
                if session.active_renderer_id.is_none() {
                    "missing_active_renderer_id"
                } else if session.local_renderer_id.is_none() {
                    "missing_local_renderer_id"
                } else {
                    "active_renderer_is_local"
                }
            };
            log::info!("[QConnect] skip {direction_label} handoff skipped: {reason}");
            dev_push_event(format!(
                "controller skip {direction_label}: local ({reason})"
            ));
            return Ok(false);
        };

        let resolution = resolve_controller_queue_item_from_snapshots(&queue, &renderer, direction);

        let Some(target_queue_item_id) = resolution.target_queue_item_id else {
            log::warn!(
                "[QConnect] skip {direction_label} handoff: no target queue item resolved (strategy={}, current_qid={:?}, next_qid={:?}, queue_version={}.{}, items={}, shuffle={}, order_present={})",
                resolution.strategy,
                renderer.current_track.as_ref().map(|item| item.queue_item_id),
                renderer.next_track.as_ref().map(|item| item.queue_item_id),
                queue.version.major, queue.version.minor,
                queue.queue_items.len() + queue.autoplay_items.len(),
                queue.shuffle_mode, queue.shuffle_order.is_some(),
            );
            dev_push_event(format!(
                "controller skip {direction_label}: NO TARGET ({})",
                resolution.strategy
            ));
            return Err(format!(
                "remote renderer active but no {direction_label} target queue item could be resolved"
            ));
        };

        let target_queue_item_id_i32 = i32::try_from(target_queue_item_id)
            .map_err(|_| format!("target queue item id out of range: {target_queue_item_id}"))?;
        // Official manual next/previous always starts playback. Previous that
        // restarts the current item is a bare seek, not a track reselection.
        let restart_current = matches!(direction, QconnectRemoteSkipDirection::Previous)
            && resolution.strategy == "restart_current_queue_item";
        let payload = serde_json::to_value(QconnectSetPlayerStateRequest {
            playing_state: Some(PLAYING_STATE_PLAYING),
            current_position: Some(0),
            current_queue_item: (!restart_current).then_some(
                QconnectSetPlayerStateQueueItemPayload {
                    queue_version: Some(QconnectQueueVersionPayload {
                        major: queue.version.major,
                        minor: queue.version.minor,
                    }),
                    id: Some(target_queue_item_id_i32),
                },
            ),
        })
        .map_err(|err| format!("serialize controller skip payload: {err}"))?;

        self.send_command(QueueCommandType::CtrlSrvrSetPlayerState, payload)
            .await?;
        self.prime_remote_renderer_state(
            target_queue_item_id,
            Some(PLAYING_STATE_PLAYING),
            Some(0),
        )
        .await;
        if let Some(target_track_id) = resolution.matched_track_id {
            self.align_local_cursor(target_track_id).await;
        }

        log::info!(
            "[QConnect] skip {direction_label} handoff -> queue_item {target_queue_item_id} (strategy={})",
            resolution.strategy
        );
        dev_push_event(format!(
            "controller skip {direction_label} -> qid {target_queue_item_id} (active={:?})",
            session.active_renderer_id
        ));

        Ok(true)
    }

    /// Toggle play/pause on the active PEER renderer. Mirrors the Tauri
    /// `toggle_remote_renderer_playback_if_active`.
    pub async fn toggle_remote_renderer_playback_if_active(&self) -> Result<bool, String> {
        self.request_remote_renderer_playback_if_active(None).await
    }

    pub(crate) async fn request_remote_renderer_playback_if_active(
        &self,
        requested: Option<bool>,
    ) -> Result<bool, String> {
        let Some(_runtime_action) = self.begin_runtime_action_if_running()? else {
            return Ok(false);
        };
        let remote_context = self.effective_remote_renderer_snapshot().await?;
        let Some((renderer, _queue, session)) = remote_context else {
            let reason = {
                let sync_state = {
                    let guard = lock_inner(&self.inner);
                    let Some(runtime) = guard.runtime.as_ref() else {
                        return Ok(false);
                    };
                    Arc::clone(&runtime.sync_state)
                };
                let session = sync_state.lock().await.session.clone();
                if session.active_renderer_id.is_none() {
                    "missing_active_renderer_id"
                } else if session.local_renderer_id.is_none() {
                    "missing_local_renderer_id"
                } else {
                    "active_renderer_is_local"
                }
            };
            log::info!("[QConnect] toggle_play handoff skipped: {reason}");
            dev_push_event(format!("controller toggle_play: local ({reason})"));
            return Ok(false);
        };

        let playing = renderer.playing_state == Some(PLAYING_STATE_PLAYING);
        let next_playing_state = if requested.unwrap_or(!playing) {
            PLAYING_STATE_PLAYING
        } else {
            PLAYING_STATE_PAUSED
        };
        // BARE play/pause: send ONLY `playing_state` — no `current_position`, no
        // `current_queue_item`. Evidence (controller-of-iOS log 2026-06-05,
        // 23:07:59): iOS ACCEPTS the pause (it reports playing_state=PAUSED) but
        // then AUTO-RESUMES to PLAYING within the same second, with NO play command
        // from QBZ in between — even though the qid (0) and queue_version (12.4) QBZ
        // sent were CORRECT (verified against iOS's own SetState). Attaching a
        // (possibly-stale) `current_position` + a `current_queue_item` makes iOS
        // treat the command as a SEEK / set-state and bounce back to playing; a
        // pure transport toggle needs neither — the renderer pauses/resumes its own
        // current item in place. WebPlayer-as-renderer (verified working) pauses
        // fine on a bare command too, so this does not regress it. (Only remote
        // VOLUME is genuinely refused by iOS.)
        let payload = serde_json::to_value(QconnectSetPlayerStateRequest {
            playing_state: Some(next_playing_state),
            current_position: None,
            current_queue_item: None,
        })
        .map_err(|err| format!("serialize toggle_play request: {err}"))?;

        self.send_command(QueueCommandType::CtrlSrvrSetPlayerState, payload)
            .await?;
        self.prime_remote_renderer_playing_state(next_playing_state)
            .await;

        log::info!("[QConnect] toggle_play handoff -> playing_state {next_playing_state}");
        dev_push_event(format!(
            "controller toggle_play -> {next_playing_state} (active={:?})",
            session.active_renderer_id
        ));

        Ok(true)
    }

    /// Select an existing visible upcoming occurrence without replacing the
    /// remote queue. The index is validated against the materialized projection
    /// so duplicate catalog ids retain their distinct cloud queue-item ids.
    pub async fn play_remote_upcoming_if_active(
        &self,
        upcoming_index: usize,
        expected_track_id: u64,
    ) -> Result<bool, String> {
        let Some(_runtime_action) = self.begin_runtime_action_if_running()? else {
            return Ok(false);
        };
        // Keep the common playback funnel's Cast-first precedence.
        if crate::cast_qt::is_casting().await {
            return Ok(false);
        }
        // D8: an offline-only playlist retains its local playback route even
        // when catalog ids happen to match an existing cloud queue.
        if self.runtime.core().queue_is_offline_only() {
            return Ok(false);
        }
        let Some((renderer, queue, _session)) = self.effective_remote_renderer_snapshot().await?
        else {
            return Ok(false);
        };
        let (tracks, _) = self.runtime.core().get_all_queue_tracks().await;
        let local = self.runtime.core().get_queue_state_full().await;
        if !local_upcoming_matches_remote(&queue, &renderer, &tracks, &local) {
            return Err("remote queue projection is not ready for selection".to_string());
        }
        let request =
            remote_upcoming_selection(&queue, &renderer, upcoming_index, expected_track_id)
                .ok_or_else(|| "remote upcoming selection changed or is unavailable".to_string())?;
        let target_qid = request
            .current_queue_item
            .as_ref()
            .and_then(|item| item.id)
            .expect("validated upcoming selection has an item") as u64;
        let payload = serde_json::to_value(request)
            .map_err(|error| format!("serialize remote upcoming selection: {error}"))?;
        self.send_command(QueueCommandType::CtrlSrvrSetPlayerState, payload)
            .await?;
        // Acceptance by the transport is not acceptance by the renderer.
        // Keep both local and peer cursors unchanged until authoritative echo.
        // Do not align the core by catalog id: it would select the first of
        // duplicate occurrences. The authoritative peer echo owns that cursor.
        log::info!("[QConnect] selected remote upcoming item {target_qid}");
        Ok(true)
    }

    /// Hand off a "play this track" to the active PEER renderer. Polls the cloud
    /// queue until the track appears, then SetPlayerState to it. Mirrors the
    /// Tauri `play_remote_renderer_track_if_active`.
    pub async fn play_remote_renderer_track_if_active(
        &self,
        track_id: u64,
    ) -> Result<bool, String> {
        let Some(_runtime_action) = self.begin_runtime_action_if_running()? else {
            return Ok(false);
        };
        let (app, sync_state) = {
            let guard = lock_inner(&self.inner);
            let Some(runtime) = guard.runtime.as_ref() else {
                return Ok(false);
            };
            (Arc::clone(&runtime.app), Arc::clone(&runtime.sync_state))
        };
        let session = sync_state.lock().await.session.clone();

        let active_renderer_id = session.active_renderer_id;
        let local_renderer_id = session.local_renderer_id;
        let early_return_reason = if active_renderer_id.is_none() {
            Some("missing_active_renderer_id")
        } else if local_renderer_id.is_none() {
            Some("missing_local_renderer_id")
        } else if active_renderer_id == local_renderer_id {
            Some("active_renderer_is_local")
        } else {
            None
        };

        if let Some(reason) = early_return_reason {
            if reason == "active_renderer_is_local" {
                let mut state = sync_state.lock().await;
                state.last_load_attempt = Some((track_id, std::time::Instant::now()));
            }
            log::info!("[QConnect] play_track handoff skipped: {reason} (track {track_id})");
            dev_push_event(format!(
                "controller play_track {track_id}: local ({reason})"
            ));
            return Ok(false);
        }

        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_millis(QCONNECT_PLAY_TRACK_HANDOFF_WAIT_MS);
        let poll_interval = std::time::Duration::from_millis(QCONNECT_PLAY_TRACK_HANDOFF_POLL_MS);
        let mut attempts: u32 = 0;
        loop {
            attempts += 1;
            let queue = app.queue_state_snapshot().await;

            let (resolved_queue_item_id, _, _) =
                resolve_queue_item_ids_from_queue_state(&queue, track_id);

            if let Some(target_queue_item_id) = resolved_queue_item_id {
                let target_queue_item_id_i32 =
                    i32::try_from(target_queue_item_id).map_err(|_| {
                        format!("target queue item id out of range: {target_queue_item_id}")
                    })?;

                let payload = serde_json::to_value(QconnectSetPlayerStateRequest {
                    playing_state: Some(PLAYING_STATE_PLAYING),
                    current_position: Some(0),
                    current_queue_item: Some(QconnectSetPlayerStateQueueItemPayload {
                        queue_version: Some(QconnectQueueVersionPayload {
                            major: queue.version.major,
                            minor: queue.version.minor,
                        }),
                        id: Some(target_queue_item_id_i32),
                    }),
                })
                .map_err(|err| format!("serialize play_track handoff payload: {err}"))?;

                self.send_command(QueueCommandType::CtrlSrvrSetPlayerState, payload)
                    .await?;
                self.prime_remote_renderer_state(
                    target_queue_item_id,
                    Some(PLAYING_STATE_PLAYING),
                    Some(0),
                )
                .await;
                self.align_local_cursor(track_id).await;

                log::info!(
                    "[QConnect] play_track handoff -> qid {target_queue_item_id} (track {track_id}, attempts={attempts})"
                );
                dev_push_event(format!(
                    "controller play_track {track_id} -> qid {target_queue_item_id}"
                ));

                return Ok(true);
            }

            if tokio::time::Instant::now() >= deadline {
                break;
            }

            tokio::time::sleep(poll_interval).await;
        }

        log::warn!(
            "[QConnect] play_track handoff: track {track_id} not present in remote queue after {QCONNECT_PLAY_TRACK_HANDOFF_WAIT_MS}ms"
        );
        dev_push_event(format!(
            "controller play_track {track_id}: NOT IN QUEUE (timeout)"
        ));
        Err(format!(
            "remote renderer active but track {track_id} was not present in qconnect queue after {QCONNECT_PLAY_TRACK_HANDOFF_WAIT_MS}ms"
        ))
    }

    /// True whenever a QConnect transport/session is established (renderer OR
    /// controller). Mirrors the Tauri `status().transport_connected` gate that
    /// `v2_toggle_shuffle` / `v2_set_repeat_mode` use: shuffle/repeat are
    /// QUEUE-state operations the cloud OWNS, so they go to the cloud whenever
    /// connected, regardless of who is the active renderer.
    async fn transport_connected(&self) -> bool {
        let app = {
            let guard = lock_inner(&self.inner);
            match guard.runtime.as_ref() {
                Some(runtime) => Arc::clone(&runtime.app),
                None => return false,
            }
        };
        app.state_handle().lock().await.transport_connected
    }

    /// Toggle shuffle through the CLOUD whenever connected (renderer OR
    /// controller) — exactly like Tauri `v2_toggle_shuffle`, which gates on
    /// `transport_connected`, NOT on a peer being active.
    ///
    /// WS-AUTHORITATIVE (load-bearing): QBZ sends ONLY `{shuffle_mode,
    /// shuffle_seed, shuffle_pivot_queue_item_id}` — never a local order. The
    /// server-authorized seed/pivot or `shuffled_track_indexes` is the sole
    /// input to the deterministic playback order applied by every client.
    /// Inbound `SetShuffleMode` alone cannot mutate the local queue. The local
    /// `playback::toggle_shuffle` path (which invents local entropy) is reachable
    /// ONLY when NOT connected (this returns `Ok(false)` then, so the caller runs
    /// it offline). The previous peer-only gate let that local path run while
    /// connected-as-renderer, producing the documented divergent-order bug.
    pub async fn toggle_shuffle_if_remote(&self) -> Result<bool, String> {
        let Some(_runtime_action) = self.begin_runtime_action_if_running()? else {
            return Ok(false);
        };
        if !self.transport_connected().await {
            // Offline: caller runs the local shuffle path.
            return Ok(false);
        }
        // UN-gated snapshot: returns Some even when QBZ ITSELF is the active
        // renderer (effective_remote_* returns None then). Mirrors Tauri
        // queue_snapshot()/renderer_snapshot().
        let Some((renderer, queue, session)) = self.effective_active_renderer_snapshot().await?
        else {
            // Connected but no active renderer yet — do NOT fall through to the
            // local reshuffle (it would diverge from the cloud). Handled no-op.
            return Ok(true);
        };

        // Toggle from the cloud-authoritative QUEUE shuffle flag (matches Tauri
        // `!queue.shuffle_mode`), not the per-renderer field which the cloud
        // never populates for a peer.
        let next_shuffle = !queue.shuffle_mode;

        // The server REQUIRES a `shuffle_seed` when enabling ("shuffleSeed is
        // undefined" otherwise). QBZ originates one only while acting as the
        // controller that requested this toggle; renderers consume the echoed
        // WS seed/pivot and never generate another. No `rand` crate here (unlike
        // Tauri); seed from the wall clock, masked to i32::MAX for the wire
        // `fixed32`. Pivot keeps the current track at the front. Mirrors the
        // Tauri `apply_qconnect_shuffle_mode` payload.
        let shuffle_seed: Option<u32> = next_shuffle.then(|| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u32)
                .unwrap_or(1);
            nanos & (i32::MAX as u32)
        });
        let pivot_queue_item_id =
            qconnect_app::queue_resolution::resolve_qconnect_shuffle_pivot(&queue, &renderer);

        let payload = json!({
            "shuffle_mode": next_shuffle,
            "shuffle_seed": shuffle_seed.map(i64::from),
            "shuffle_pivot_queue_item_id": pivot_queue_item_id
                .and_then(|value| i32::try_from(value).ok())
                .map(i64::from),
            "autoplay_reset": false,
            "autoplay_loading": false,
        });
        self.send_command(QueueCommandType::CtrlSrvrSetShuffleMode, payload)
            .await?;

        log::info!(
            "[QConnect] shuffle -> {next_shuffle} (cloud, active={:?})",
            session.active_renderer_id
        );
        dev_push_event(format!(
            "shuffle -> {next_shuffle} (active={:?})",
            session.active_renderer_id
        ));

        Ok(true)
    }

    /// Cycle repeat through the CLOUD whenever connected (renderer OR
    /// controller) — like Tauri `v2_set_repeat_mode` (gate = transport_connected).
    /// QConnect loop wire values: 1=off, 3=all, 2=one; cycle off->all->one->off.
    /// Returns `Ok(false)` ONLY when NOT connected so the caller runs local.
    pub async fn cycle_repeat_if_remote(&self) -> Result<bool, String> {
        let Some(_runtime_action) = self.begin_runtime_action_if_running()? else {
            return Ok(false);
        };
        if !self.transport_connected().await {
            return Ok(false);
        }
        let Some((renderer, _queue, session)) = self.effective_active_renderer_snapshot().await?
        else {
            return Ok(true);
        };

        let current_loop = renderer.loop_mode.unwrap_or(1);
        let next_loop = match current_loop {
            0 | 1 => 3, // off -> all
            3 => 2,     // all -> one
            _ => 1,     // one -> off
        };

        let payload = json!({ "loop_mode": next_loop });
        self.send_command(QueueCommandType::CtrlSrvrSetLoopMode, payload)
            .await?;

        log::info!(
            "[QConnect] repeat -> {next_loop} (cloud, active={:?})",
            session.active_renderer_id
        );
        dev_push_event(format!(
            "repeat -> {next_loop} (active={:?})",
            session.active_renderer_id
        ));

        Ok(true)
    }

    /// Reorder the upcoming queue through the CLOUD whenever connected (renderer
    /// OR controller) — like Tauri `v2_move_queue_track` (gate = transport_connected,
    /// NOT peer-active; queue order is cloud-owned). WS-AUTHORITATIVE: QBZ sends
    /// only `{queue_item_ids:[moved], insert_after}`; the cloud reorders and echoes
    /// a QueueUpdated that materialize applies. The local `move_track` path runs
    /// ONLY when NOT connected (this returns `Ok(false)` then).
    ///
    /// `from_q` / `to_q` are queue-wide UPCOMING indices (0 = first upcoming).
    pub async fn reorder_upcoming_if_remote(
        &self,
        from_q: usize,
        to_q: usize,
    ) -> Result<bool, String> {
        let Some(_runtime_action) = self.begin_runtime_action_if_running()? else {
            return Ok(false);
        };
        if !self.transport_connected().await {
            return Ok(false);
        }
        // UN-gated snapshot (Some even when QBZ itself is the active renderer),
        // matching toggle_shuffle_if_remote.
        let Some((renderer, queue, session)) = self.effective_active_renderer_snapshot().await?
        else {
            // Connected but no active renderer yet — handled no-op (do NOT fall
            // through to a local reorder that would diverge from the cloud).
            return Ok(true);
        };

        let projection = build_visible_upcoming_projection(&queue, &renderer);
        let len = projection.upcoming_qids.len();
        if len == 0 {
            return Ok(true);
        }
        // Clamp into the projection's [0, len) index space (the core path may pass
        // to_q == len for an append-to-end slot).
        let from_index = from_q.min(len - 1);
        let to_index = to_q.min(len - 1);
        if from_index == to_index {
            return Ok(true);
        }

        let Some(payload) = build_reorder_payload(&projection, from_index, to_index) else {
            return Ok(true); // out of range / nothing to do — handled
        };

        self.send_command(QueueCommandType::CtrlSrvrQueueReorderTracks, payload)
            .await?;

        log::info!(
            "[QConnect] reorder upcoming {from_index} -> {to_index} (cloud, active={:?})",
            session.active_renderer_id
        );
        dev_push_event(format!(
            "reorder {from_index} -> {to_index} (active={:?})",
            session.active_renderer_id
        ));
        Ok(true)
    }

    /// Set volume on the active PEER renderer. Special case: if the renderer
    /// disallows remote volume, return `Ok(true)` (handled no-op) so the
    /// frontend does NOT fall back to local volume. Mirrors the Tauri
    /// `set_volume_if_remote`.
    pub async fn set_volume_if_remote(&self, volume: i32) -> Result<bool, String> {
        let Some(_runtime_action) = self.begin_runtime_action_if_running()? else {
            return Ok(false);
        };
        let remote_context = self.effective_remote_renderer_snapshot().await?;
        let Some((_renderer, _queue, session)) = remote_context else {
            return Ok(false);
        };

        if let Some(active_id) = session.active_renderer_id {
            if let Some(info) = session
                .renderers
                .iter()
                .find(|r| r.renderer_id == active_id)
            {
                if !renderer_allows_remote_volume(info) {
                    log::info!(
                        "[QConnect] set_volume_if_remote short-circuited: renderer {active_id} disallows remote volume"
                    );
                    dev_push_event(
                        "controller volume: renderer disallows remote volume (no-op)".to_string(),
                    );
                    return Ok(true);
                }
            }
        }

        let payload = serde_json::to_value(QconnectSetVolumeRequest {
            renderer_id: session.active_renderer_id,
            volume: Some(volume),
            volume_delta: None,
        })
        .map_err(|err| format!("serialize set_volume request: {err}"))?;

        self.send_command(QueueCommandType::CtrlSrvrSetVolume, payload)
            .await?;

        log::info!("[QConnect] set_volume handoff -> {volume}");
        dev_push_event(format!(
            "controller volume -> {volume} (active={:?})",
            session.active_renderer_id
        ));

        Ok(true)
    }

    /// Toggle the active peer's mute, independently of the owner's local mute.
    pub async fn toggle_mute_if_remote(&self) -> Result<bool, String> {
        let Some(_runtime_action) = self.begin_runtime_action_if_running()? else {
            return Ok(false);
        };
        let remote_context = self.effective_remote_renderer_snapshot().await?;
        let Some((renderer, _queue, session)) = remote_context else {
            return Ok(false);
        };
        let value = !renderer.muted.unwrap_or(false);

        let payload = serde_json::to_value(QconnectMuteVolumeRequest {
            renderer_id: session.active_renderer_id,
            value,
        })
        .map_err(|err| format!("serialize mute_volume request: {err}"))?;

        self.send_command(QueueCommandType::CtrlSrvrMuteVolume, payload)
            .await?;

        let sync_state = {
            let guard = lock_inner(&self.inner);
            guard
                .runtime
                .as_ref()
                .map(|runtime| Arc::clone(&runtime.sync_state))
        };
        if let (Some(sync_state), Some(renderer_id)) = (sync_state, session.active_renderer_id) {
            let mut sync = sync_state.lock().await;
            if project_peer_mute(&mut sync, renderer_id, value) {
                crate::now_playing::set_muted(value);
            }
        }

        log::info!("[QConnect] mute handoff -> {value}");
        dev_push_event(format!(
            "controller mute -> {value} (active={:?})",
            session.active_renderer_id
        ));

        Ok(true)
    }

    /// Set autoplay mode on the active PEER renderer. Mirrors the Tauri
    /// `set_autoplay_mode_if_remote`.
    // Reserved: ported QConnect transport API, pending wiring by the QConnect
    // autoplay/stop UI work.
    #[allow(dead_code)]
    pub async fn set_autoplay_mode_if_remote(&self, enabled: bool) -> Result<bool, String> {
        let Some(_runtime_action) = self.begin_runtime_action_if_running()? else {
            return Ok(false);
        };
        let remote_context = self.effective_remote_renderer_snapshot().await?;
        let Some((_renderer, _queue, session)) = remote_context else {
            return Ok(false);
        };

        let payload = json!({
            "autoplay_mode": enabled,
            "autoplay_reset": true,
            "autoplay_loading": false
        });
        self.send_command(QueueCommandType::CtrlSrvrSetAutoplayMode, payload)
            .await?;

        log::info!("[QConnect] set_autoplay_mode handoff -> {enabled}");
        dev_push_event(format!(
            "controller autoplay -> {enabled} (active={:?})",
            session.active_renderer_id
        ));

        Ok(true)
    }

    /// Load autoplay tracks onto the active PEER renderer. Empty list = handled
    /// no-op. Mirrors the Tauri `autoplay_load_tracks_if_remote`.
    #[allow(dead_code)] // Reserved: ported, pending QConnect autoplay wiring.
    pub async fn autoplay_load_tracks_if_remote(
        &self,
        track_ids: Vec<u32>,
    ) -> Result<bool, String> {
        let Some(_runtime_action) = self.begin_runtime_action_if_running()? else {
            return Ok(false);
        };
        let remote_context = self.effective_remote_renderer_snapshot().await?;
        let Some((_renderer, _queue, session)) = remote_context else {
            return Ok(false);
        };

        if track_ids.is_empty() {
            return Ok(true); // nothing to load, but handled remotely
        }

        let track_count = track_ids.len();
        let payload = json!({
            "track_ids": track_ids,
            "context_uuid": Uuid::new_v4().to_string()
        });
        self.send_command(QueueCommandType::CtrlSrvrAutoplayLoadTracks, payload)
            .await?;

        log::info!("[QConnect] autoplay_load_tracks handoff -> {track_count} tracks");
        dev_push_event(format!(
            "controller autoplay_load {track_count} (active={:?})",
            session.active_renderer_id
        ));

        Ok(true)
    }

    /// Stop the active PEER renderer. Mirrors the Tauri `stop_if_remote`.
    #[allow(dead_code)] // Reserved: ported, pending QConnect stop wiring.
    pub async fn stop_if_remote(&self) -> Result<bool, String> {
        let Some(_runtime_action) = self.begin_runtime_action_if_running()? else {
            return Ok(false);
        };
        let remote_context = self.effective_remote_renderer_snapshot().await?;
        let Some((renderer, queue, session)) = remote_context else {
            return Ok(false);
        };

        let current_position = renderer
            .current_position_ms
            .and_then(|value| i32::try_from(value).ok());
        let current_queue_item = renderer.current_track.as_ref().and_then(|item| {
            i32::try_from(item.queue_item_id).ok().map(|queue_item_id| {
                QconnectSetPlayerStateQueueItemPayload {
                    queue_version: Some(QconnectQueueVersionPayload {
                        major: queue.version.major,
                        minor: queue.version.minor,
                    }),
                    id: Some(queue_item_id),
                }
            })
        });

        let payload = serde_json::to_value(QconnectSetPlayerStateRequest {
            playing_state: Some(PLAYING_STATE_STOPPED),
            current_position,
            current_queue_item,
        })
        .map_err(|err| format!("serialize stop request: {err}"))?;

        self.send_command(QueueCommandType::CtrlSrvrSetPlayerState, payload)
            .await?;
        self.prime_remote_renderer_playing_state(PLAYING_STATE_STOPPED)
            .await;

        log::info!("[QConnect] stop handoff");
        dev_push_event(format!(
            "controller stop (active={:?})",
            session.active_renderer_id
        ));

        Ok(true)
    }

    /// Seek against the peer's current track metadata, never the stopped local
    /// audio engine's duration. Preserve the captured peer track/version and
    /// leave playing_state absent so seeking cannot toggle play/pause.
    pub async fn seek_fraction_if_remote(&self, fraction: f32) -> Result<bool, String> {
        let Some(_runtime_action) = self.begin_runtime_action_if_running()? else {
            return Ok(false);
        };
        let remote_context = self.effective_remote_renderer_snapshot().await?;
        let Some((renderer, queue, session)) = remote_context else {
            return Ok(false);
        };

        let current_track = renderer
            .current_track
            .as_ref()
            .ok_or_else(|| "remote renderer current track is unknown".to_string())?;
        let (tracks, _) = self.runtime.core().get_all_queue_tracks().await;
        let position_ms = peer_seek_position_ms(fraction, current_track.track_id, &tracks)
            .ok_or_else(|| {
                "remote renderer track duration or seek position is unavailable".to_string()
            })?;

        let request = build_set_position_player_state_request(
            position_ms,
            Some(current_track.queue_item_id),
            QconnectQueueVersionPayload {
                major: queue.version.major,
                minor: queue.version.minor,
            },
        );
        let payload = serde_json::to_value(request)
            .map_err(|err| format!("serialize set_position request: {err}"))?;

        self.send_command(QueueCommandType::CtrlSrvrSetPlayerState, payload)
            .await?;

        let sync_state = {
            let guard = lock_inner(&self.inner);
            guard
                .runtime
                .as_ref()
                .map(|runtime| Arc::clone(&runtime.sync_state))
        };
        if let (Some(sync_state), Some(renderer_id)) = (sync_state, session.active_renderer_id) {
            let mut sync = sync_state.lock().await;
            project_peer_seek(
                &mut sync,
                renderer_id,
                current_track.queue_item_id,
                position_ms as u64,
                qconnect_now_ms(),
            );
        }

        log::info!("[QConnect] set_position handoff -> {position_ms}ms");
        dev_push_event(format!(
            "controller seek -> {position_ms}ms (active={:?})",
            session.active_renderer_id
        ));

        Ok(true)
    }

    /// Switch the active renderer (device picker / "Play here"). Thin wrapper
    /// over `QconnectApp::send_set_active_renderer` (guard + clear-pending).
    /// Mirrors the Tauri `v2_qconnect_set_active_renderer`.
    pub async fn set_active_renderer(&self, renderer_id: i32) -> Result<bool, String> {
        let _runtime_action = self.begin_runtime_action()?;
        let app = {
            let guard = lock_inner(&self.inner);
            guard
                .runtime
                .as_ref()
                .map(|runtime| Arc::clone(&runtime.app))
                .ok_or_else(|| "QConnect service is not running".to_string())?
        };
        let handled = app.send_set_active_renderer(renderer_id).await?;
        dev_push_event(format!(
            "controller set_active_renderer -> {renderer_id} (sent={handled})"
        ));
        Ok(handled)
    }
}

/// B12 (offline-MODE): park the calling session-loop seam while the offline
/// engine reports offline, waking ONLY on the online edge — event-driven via
/// the engine's watch channel, no polling/sleeping. Session teardown needs no
/// extra signal: the D5 force-disconnect watcher's `disconnect()` aborts the
/// session-loop task (`runtime.event_loop.abort()`), which cancels this future
/// at the `.await`. Per D5 this never triggers a reconnect by itself — when the
/// online edge arrives with the session still alive, the caller simply resumes
/// its normal behavior. MUST be called WITHOUT holding any service lock (it
/// parks indefinitely; `disconnect()` needs the `inner` lock to tear down).
async fn park_session_loop_while_offline(context: &str) {
    let mut rx = crate::offline_fwd::engine().subscribe();
    if !rx.borrow_and_update().is_offline() {
        return;
    }
    log::info!(
        "[QConnect] {context}: offline mode active; parking the session loop until the online edge or teardown (B12)"
    );
    loop {
        if rx.changed().await.is_err() {
            // Engine sender dropped (process teardown) — nothing left to gate.
            return;
        }
        if !rx.borrow_and_update().is_offline() {
            break;
        }
    }
    log::info!("[QConnect] {context}: online edge observed; resuming the session loop (B12)");
}

/// Qt-side implementation of the shared session-loop seams. Holds the handles
/// the loop reaches back into: the app (renderer join + state reads), the
/// shared sync accumulator, the service inner (lifecycle gating + teardown),
/// the sink (lifecycle emit), and the runtime (track duration read for the
/// join).
pub(crate) struct QtSessionLoopHost {
    pub(crate) app: Arc<QtQconnectApp>,
    pub(crate) sync_state: Arc<Mutex<QconnectRemoteSyncState>>,
    pub(crate) inner: Arc<StdMutex<QtQconnectInner>>,
    pub(crate) authority: Arc<AuthorityCell>,
    pub(crate) stamp: AuthorityStamp,
    pub(crate) sink: Arc<QtQconnectEventSink>,
    pub(crate) runtime: Runtime,
    pub(crate) projection: QtLanProjectionSlot,
}

#[async_trait::async_trait]
impl SessionLoopHost for QtSessionLoopHost {
    fn local_playback_is_playing(&self) -> bool {
        self.authority.is_current(self.stamp) && self.runtime.core().get_playback_state().is_playing
    }

    async fn publish_local_queue_for_takeover(&self) -> bool {
        publish_local_queue_for_takeover(
            &self.app,
            &self.sync_state,
            &self.inner,
            &self.runtime,
            &self.authority,
            self.stamp,
        )
        .await
    }

    async fn resolve_local_playback_conflict(
        &self,
        active_renderer_id: i32,
        _peer_was_playing: bool,
    ) -> LocalPlaybackConflictChoice {
        if !self.authority.is_current(self.stamp) {
            return LocalPlaybackConflictChoice::CancelConnection;
        }
        let policy = crate::qconnect_transport_qt::load_playback_conflict_policy();
        if let Some(choice) = policy.automatic_choice() {
            log::info!(
                "[QConnect] Local playback conflicts with active renderer {active_renderer_id}; applying persisted policy {}",
                policy.as_str()
            );
            return choice;
        }
        let renderer_name = {
            let state = self.sync_state.lock().await;
            state
                .session
                .renderers
                .iter()
                .find(|renderer| renderer.renderer_id == active_renderer_id)
                .and_then(|renderer| renderer.friendly_name.clone())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "Qobuz Connect".to_string())
        };
        log::info!(
            "[QConnect] Local playback conflicts with active renderer {active_renderer_id}; awaiting user choice"
        );
        request_playback_conflict_choice(renderer_name).await
    }

    async fn stop_local_playback_for_remote_queue(&self) {
        if !self.authority.is_current(self.stamp) {
            return;
        }
        if let Err(error) = self.runtime.core().stop() {
            log::warn!("[QConnect] Failed to stop local playback for remote queue: {error}");
        }
    }

    async fn continue_active_renderer_playback(&self) {
        if !self.authority.is_current(self.stamp) {
            return;
        }
        let peer_is_playing = {
            let state = self.sync_state.lock().await;
            active_peer_renderer_is_playing(&state)
        };
        if peer_is_playing {
            return;
        }
        let Some(service) = service() else {
            return;
        };
        if let Err(error) = service.toggle_remote_renderer_playback_if_active().await {
            log::warn!("[QConnect] Failed to continue active renderer: {error}");
        }
    }

    async fn cancel_connection_for_playback_conflict(&self) {
        publish::playback_conflict(false, String::new());
        let Some(service) = service() else {
            return;
        };
        crate::spawn(async move {
            match service.disconnect_safely().await {
                Ok(outcome) if outcome.authority_safe => {
                    publish::connected(false);
                    let _ = tokio::task::spawn_blocking(|| {
                        if crate::qconnect_transport_qt::load_startup_mode()
                            == qconnect_app::QconnectStartupMode::RememberLast
                        {
                            crate::qconnect_transport_qt::save_last_known_state(false);
                        }
                    })
                    .await;
                }
                Ok(_) => log::warn!(
                    "[QConnect] Playback-conflict cancellation left authority teardown incomplete"
                ),
                Err(error) => {
                    log::warn!("[QConnect] Playback-conflict cancellation failed: {error}")
                }
            }
        });
    }

    async fn update_lifecycle(&self, state: QconnectLifecycleState) {
        update_lifecycle_state_if_running(
            &self.inner,
            &self.sink,
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
        // D5 (offline-MODE): never re-bootstrap presence while offline. The
        // force-disconnect watcher is tearing the session down on the offline
        // edge; a transport reconnect that sneaks in before that lands (induced
        // offline keeps the network up) must stay dormant, not re-join.
        // B12: the suppression is event-driven — instead of skipping (which
        // left a subscribed-but-never-bootstrapped session dormant forever if
        // the watcher's teardown raced or failed), park on the offline
        // engine's watch channel. Teardown aborts the loop task (cancelling
        // this wait); if the session is still alive when the online edge
        // arrives, the normal bootstrap below resumes.
        park_session_loop_while_offline("Reconnect bootstrap").await;
        if !self.authority.is_current(self.stamp) {
            return;
        }
        // Refresh the owner token/config for subsequent reconnect attempts. A
        // long-running desktop session must not keep retrying an expired JWT.
        match resolve_transport_config(&self.runtime).await {
            Ok(fresh) => {
                if !self.authority.is_current(self.stamp) {
                    return;
                }
                let latched = {
                    let mut guard = lock_inner(&self.inner);
                    if let Some(runtime) = guard.runtime.as_mut() {
                        if runtime.stamp == self.stamp {
                            runtime.config = fresh;
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
                log::info!("[QConnect] reconnect: refreshed owner transport credentials");
            }
            Err(error) => {
                if !self.authority.is_current(self.stamp) {
                    return;
                }
                log::warn!("[QConnect] reconnect credential refresh failed: {error}");
            }
        }
        if let Err(err) =
            bootstrap_remote_presence(&self.app, None, &self.authority, self.stamp).await
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
            let mut guard = lock_inner(&self.inner);
            if guard.runtime.as_ref().map(|runtime| runtime.stamp) != Some(self.stamp) {
                (false, None)
            } else {
                guard.lifecycle_state = QconnectLifecycleState::Exhausted;
                guard.last_error = Some(format!(
                    "Reconnect attempts exhausted ({attempts}): {last_reason}"
                ));
                if !idle_retry_active {
                    (true, guard.runtime.take())
                } else {
                    (true, None)
                }
            }
        };
        // Never drop a retired runtime while holding the service mutex.
        drop(retired);
        if !applied || !self.authority.is_current(self.stamp) {
            return true;
        }
        // TODO(qt-qconnect-ui): surface the Exhausted lifecycle on the badge.
        // (Reference TODO(slint-qconnect-ui), kept unwired per §9 D6.)
        log::warn!("[QConnect] Reconnect exhausted ({attempts}): {last_reason}");
        // B12 (offline-MODE): the reconnect backoff loop AND the 60s idle
        // rearm both live INSIDE qconnect-transport-ws
        // (`idle_retry_after_exhausted`) — the host owns no retry timer it
        // could gate, so this hook (the host-controlled point the loop hits
        // once per rearm cycle) is the correct seam. While offline, park on
        // the offline engine's watch channel instead of returning into
        // another consume-and-churn cycle: the host stops burning
        // lifecycle/lock/log cycles per rearm until either the online edge
        // (idle-retry behavior resumes — per D5 the park itself never
        // reconnects) or the D5 watcher's `disconnect()` aborts the loop task
        // (cancelling the park). The transport's internal timer keeps
        // rearming independently until that teardown lands; its events queue
        // in the broadcast channel and drain on resume (the loop's Lagged
        // recovery covers a pathologically long park). Only parked on the
        // keep-idling branch — the terminate branch breaks the loop anyway.
        if idle_retry_active {
            park_session_loop_while_offline("Idle-retry rearm").await;
        } else {
            self.projection
                .clear_if_current(&self.authority, self.stamp);
        }
        !idle_retry_active
    }

    async fn on_loop_error(&self, message: String) {
        if !self.authority.is_current(self.stamp) {
            return;
        }
        // TODO(qt-qconnect-ui): surface as a toast (Tauri emits qconnect:error).
        // (Reference TODO(slint-qconnect-ui), kept unwired per §9 D6.)
        log::error!("[QConnect] session loop error: {message}");
    }
}

const QCONNECT_RENDERER_CHANNELS: i32 = 2;
const AUDIO_QUALITY_UNKNOWN: i32 = 0;
const AUDIO_QUALITY_MP3: i32 = 1;
const AUDIO_QUALITY_CD: i32 = 2;
const AUDIO_QUALITY_HIRES_L1: i32 = 3;
const AUDIO_QUALITY_HIRES_L2: i32 = 4;
const AUDIO_QUALITY_HIRES_L3: i32 = 5;

fn qconnect_max_audio_quality_wire() -> i32 {
    match crate::playback_qt::local_playback_quality().0 {
        qbz_models::Quality::Mp3 => AUDIO_QUALITY_MP3,
        qbz_models::Quality::Lossless => AUDIO_QUALITY_CD,
        qbz_models::Quality::HiRes => AUDIO_QUALITY_HIRES_L1,
        qbz_models::Quality::UltraHiRes => AUDIO_QUALITY_HIRES_L2,
    }
}

/// Classify a (sample_rate, bit_depth) output into the QConnect AudioQuality
/// level. Pure mirror of the Tauri `classify_qconnect_audio_quality`.
fn classify_audio_quality(sample_rate: u32, bit_depth: u32) -> i32 {
    if sample_rate == 0 || bit_depth == 0 {
        AUDIO_QUALITY_UNKNOWN
    } else if sample_rate >= 384_000 {
        AUDIO_QUALITY_HIRES_L3
    } else if sample_rate >= 192_000 {
        AUDIO_QUALITY_HIRES_L2
    } else if bit_depth > 16 || sample_rate > 48_000 {
        AUDIO_QUALITY_HIRES_L1
    } else if sample_rate >= 44_100 {
        AUDIO_QUALITY_CD
    } else {
        AUDIO_QUALITY_MP3
    }
}

/// Build a file-audio-quality snapshot from the live output format, or None when
/// the format isn't known yet. Pure mirror of the Tauri
/// `build_qconnect_file_audio_quality_snapshot`.
fn build_file_audio_quality_snapshot(
    sample_rate: u32,
    bit_depth: u32,
    nb_channels: i32,
) -> Option<QconnectFileAudioQualitySnapshot> {
    if sample_rate == 0 || bit_depth == 0 {
        return None;
    }
    Some(QconnectFileAudioQualitySnapshot {
        sampling_rate: sample_rate as i32,
        bit_depth: bit_depth as i32,
        nb_channels,
        audio_quality: classify_audio_quality(sample_rate, bit_depth),
    })
}

/// Resolve the current + next `queue_item_id` for a playing `track_id` from the
/// cloud queue snapshot, caching the result into the sync accumulator. Mirrors
/// the Tauri `resolve_queue_item_ids_by_track_id`. Used by the renderer report so
/// the controller can map our playback to its queue rows.
async fn resolve_queue_item_ids_by_track_id(
    app: &Arc<QtQconnectApp>,
    sync_state: &Arc<Mutex<QconnectRemoteSyncState>>,
    track_id: u64,
) -> (Option<u64>, Option<u64>, qconnect_app::QueueVersion) {
    let queue = app.queue_state_snapshot().await;
    let (current_qid, next_qid, next_track_id) = {
        let state = sync_state.lock().await;
        qconnect_app::resolve_report_queue_items(
            &queue,
            track_id,
            state.last_renderer_queue_item_id,
            state.last_renderer_next_queue_item_id,
        )
    };

    if let Some(current_qid) = current_qid {
        let mut state = sync_state.lock().await;
        state.last_renderer_queue_item_id = Some(current_qid);
        state.last_renderer_next_queue_item_id = next_qid;
        state.last_renderer_track_id = Some(track_id);
        state.last_renderer_next_track_id = next_track_id;
        (Some(current_qid), next_qid, queue.version)
    } else {
        (None, None, queue.version)
    }
}

/// Controller-side bootstrap: JoinSession (works without a session_uuid) then ask
/// for the current queue state. The renderer-side join is deferred until the
/// server sends SESSION_STATE with a session_uuid (handled in the session loop).
/// Mirrors the Tauri `bootstrap_remote_presence`.
pub(crate) async fn bootstrap_remote_presence(
    app: &Arc<QtQconnectApp>,
    custom_device_name: Option<String>,
    authority: &AuthorityCell,
    stamp: AuthorityStamp,
) -> Result<(), String> {
    bootstrap_remote_presence_with_gate(app, custom_device_name, || authority.is_current(stamp))
        .await
}

/// Bootstrap an isolated owner candidate before its reserved stamp is
/// installed. The coordinator owns cancellation and the candidate receiver is
/// already subscribed, so emitted session events remain buffered until commit.
pub(crate) async fn bootstrap_prepared_owner_presence(
    app: &Arc<QtQconnectApp>,
    custom_device_name: Option<String>,
) -> Result<(), String> {
    bootstrap_remote_presence_with_gate(app, custom_device_name, || true).await
}

async fn bootstrap_remote_presence_with_gate<F>(
    app: &Arc<QtQconnectApp>,
    custom_device_name: Option<String>,
    is_current: F,
) -> Result<(), String>
where
    F: Fn() -> bool,
{
    if !is_current() {
        return Err("qconnect bootstrap authority retired".to_string());
    }
    let mut device_info = default_qconnect_device_info_with_name(custom_device_name.as_deref());
    if let Some(capabilities) = device_info.capabilities.as_mut() {
        capabilities.max_audio_quality = Some(qconnect_max_audio_quality_wire());
    }

    let join_payload = serde_json::to_value(QconnectJoinSessionRequest {
        session_uuid: None,
        device_info: Some(device_info),
    })
    .map_err(|err| format!("serialize join_session bootstrap payload: {err}"))?;

    let join_command = app
        .build_queue_command(QueueCommandType::CtrlSrvrJoinSession, join_payload)
        .await;
    if !is_current() {
        return Err("qconnect bootstrap authority retired".to_string());
    }
    let join_action_uuid = app
        .send_queue_command(join_command)
        .await
        .map_err(|err| format!("send bootstrap ctrl_srvr_join_session failed: {err}"))?;
    if !is_current() {
        return Err("qconnect bootstrap authority retired".to_string());
    }
    // JoinSession responds with session/renderer controller events not part of
    // queue reducer correlation. Drop the pending slot so queue ops aren't blocked.
    app.clear_pending_if_matches(&join_action_uuid).await;
    if !is_current() {
        return Err("qconnect bootstrap authority retired".to_string());
    }

    let ask_queue_command = app
        .build_queue_command(QueueCommandType::CtrlSrvrAskForQueueState, json!({}))
        .await;
    if !is_current() {
        return Err("qconnect bootstrap authority retired".to_string());
    }
    let ask_action_uuid = app
        .send_queue_command(ask_queue_command)
        .await
        .map_err(|err| format!("send bootstrap ask_for_queue_state failed: {err}"))?;
    if !is_current() {
        return Err("qconnect bootstrap authority retired".to_string());
    }
    app.clear_pending_if_matches(&ask_action_uuid).await;
    if !is_current() {
        return Err("qconnect bootstrap authority retired".to_string());
    }

    log::info!(
        "[QConnect] Bootstrap complete: controller joined, queue state requested; renderer join deferred until a session is present"
    );
    Ok(())
}

/// Deferred renderer join: called from the session loop when SESSION_STATE with a
/// session_uuid arrives. Idempotent per uuid (P1-8). Mirrors the Tauri
/// `deferred_renderer_join`, reading the current track duration via
/// `runtime.core().get_track` instead of the Tauri CoreBridge.
async fn deferred_renderer_join(
    app: &Arc<QtQconnectApp>,
    sync_state: &Arc<Mutex<QconnectRemoteSyncState>>,
    runtime: &Runtime,
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

    let mut device_info = default_qconnect_device_info();
    if let Some(capabilities) = device_info.capabilities.as_mut() {
        capabilities.max_audio_quality = Some(qconnect_max_audio_quality_wire());
    }
    if !authority.is_current(stamp) {
        return;
    }

    log::info!("[QConnect] Starting deferred renderer join");

    // 1. Renderer JoinSession with session_uuid.
    // Do NOT auto-steal the render on a fresh connect: join as an AVAILABLE
    // renderer (is_active=false), not the active one. Joining with is_active=true
    // on every connect made QBZ grab playback from whatever peer was rendering
    // the instant it came online (the "se robó solo apenas lo encendí" behavior),
    // and the self-state echo from the post-join AskForRendererState then reset
    // the cursor to the queue head. Taking over is now explicit (the device
    // picker's "Play here", or the phone selecting QBZ — both arrive as a
    // SET_ACTIVE command). Only a post-drop RECONNECTION rejoins as active, so a
    // network blip mid-render does not lose the render.
    let join_as_active = join_reason == qconnect_app::JOIN_SESSION_REASON_RECONNECTION;
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
    let volume_pct =
        (runtime.core().get_playback_state().volume.clamp(0.0, 1.0) * 100.0).round() as i32;
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
        json!({ "max_audio_quality": qconnect_max_audio_quality_wire() }),
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
    // (including ourselves). Without this, the UI may not see QBZ as a renderer
    // until the next reconnect cycle.
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

#[cfg(test)]
mod tests {
    use super::{
        active_renderer_projection, is_qconnect_queue_track, local_upcoming_matches_remote,
        peer_seek_position_ms, project_peer_mute, project_peer_seek, remote_upcoming_selection,
        resolvable_queue_projection, resolvable_track_ids,
    };
    use qbz_models::QueueTrack;
    use qconnect_app::{
        ensure_session_renderer_state, QConnectQueueState, QConnectRendererState,
        QconnectRemoteSyncState,
    };

    #[test]
    fn active_peer_never_inherits_a_stale_local_renderer_snapshot() {
        let queue = QConnectQueueState {
            queue_items: vec![serde_json::from_value(serde_json::json!({
                "queue_item_id": 0,
                "track_id": 100,
                "track_context_uuid": "",
            }))
            .expect("queue item fixture")],
            ..Default::default()
        };
        let local = QConnectRendererState {
            current_track: Some(queue.queue_items[0].clone()),
            playing_state: Some(2),
            volume: Some(100),
            muted: Some(true),
            ..Default::default()
        };
        let mut sync = peer_sync();
        for qid in [None, Some(99)] {
            let peer = ensure_session_renderer_state(&mut sync, 2);
            peer.current_queue_item_id = qid;
            peer.volume = None;
            peer.muted = None;
            let projection = active_renderer_projection(&queue, &local, &sync);
            assert!(projection.current_track.is_none());
            assert!(projection.next_track.is_none());
            assert_eq!(projection.volume, None);
            assert_eq!(projection.muted, None);
        }
        sync.session_renderer_states.clear();
        let projection = active_renderer_projection(&queue, &local, &sync);
        assert!(projection.current_track.is_none());
        assert_eq!(projection.playing_state, None);
        sync.session.active_renderer_id = Some(1);
        let projection = active_renderer_projection(&queue, &local, &sync);
        assert_eq!(projection.current_track, local.current_track);
        assert_eq!(projection.volume, Some(100));
        assert_eq!(projection.muted, Some(true));
    }

    fn track(source: Option<&str>, is_local: bool) -> QueueTrack {
        QueueTrack {
            id: 42,
            title: String::new(),
            version: None,
            artist: String::new(),
            album: String::new(),
            album_version: None,
            duration_secs: 0,
            artwork_url: None,
            hires: false,
            bit_depth: None,
            sample_rate: None,
            is_local,
            album_id: None,
            artist_id: None,
            streamable: true,
            source: source.map(str::to_string),
            parental_warning: false,
            source_item_id_hint: None,
            context_kind: None,
            context_id: None,
            isrc: None,
            recording_mbid: None,
        }
    }

    fn peer_sync() -> QconnectRemoteSyncState {
        let mut sync = QconnectRemoteSyncState::default();
        sync.session.local_renderer_id = Some(1);
        sync.session.active_renderer_id = Some(2);
        let peer = ensure_session_renderer_state(&mut sync, 2);
        peer.current_queue_item_id = Some(0);
        peer.current_position_ms = Some(3_000);
        peer.playing_state = Some(3);
        peer.muted = Some(false);
        peer.updated_at_ms = 100;
        sync
    }

    fn remote_selection_fixture() -> (QConnectQueueState, QConnectRendererState) {
        let mut queue = QConnectQueueState::default();
        queue.queue_items = [100, 200, 200, 300]
            .iter()
            .enumerate()
            .map(|(index, track_id)| {
                serde_json::from_value(serde_json::json!({
                    "track_context_uuid": "",
                    "track_id": track_id,
                    "queue_item_id": index + 10,
                }))
                .expect("queue item fixture")
            })
            .collect();
        let renderer = QConnectRendererState {
            current_track: Some(queue.queue_items[0].clone()),
            ..Default::default()
        };
        (queue, renderer)
    }

    fn local_selection_fixture(
        queue: &QConnectQueueState,
    ) -> (Vec<QueueTrack>, qbz_models::QueueState) {
        let tracks: Vec<_> = queue
            .queue_items
            .iter()
            .map(|item| {
                let mut row = track(Some("qobuz_connect_remote"), false);
                row.id = item.track_id;
                row
            })
            .collect();
        let order = queue
            .shuffle_order
            .clone()
            .unwrap_or_else(|| (0..tracks.len()).collect());
        let local = qbz_models::QueueState {
            current_track: Some(tracks[0].clone()),
            current_index: Some(0),
            upcoming: order
                .iter()
                .skip(1)
                .map(|index| tracks[*index].clone())
                .collect(),
            history: Vec::new(),
            shuffle: queue.shuffle_mode,
            repeat: Default::default(),
            total_tracks: tracks.len(),
            stop_after_track_id: None,
            manual_next_count: 0,
        };
        (tracks, local)
    }

    #[test]
    fn first_remote_next_batch_remains_the_anchor_for_later_batches() {
        let (mut queue, renderer) = remote_selection_fixture();
        let mut manual = super::ControllerManualBlock::default();
        let current = renderer.current_track.as_ref().unwrap().queue_item_id;
        let make = |track_id, queue_item_id| {
            let mut item = renderer.current_track.as_ref().unwrap().clone();
            item.track_id = track_id;
            item.queue_item_id = queue_item_id;
            item
        };
        manual.note_sent(&renderer, &queue, Some(current), vec![10, 11]);
        queue.queue_items.splice(1..1, [make(10, 20), make(11, 21)]);
        queue.version.minor += 1;
        manual.reconcile(&renderer, &queue);
        assert_eq!(manual.later_anchor(&renderer, &queue), Some(21));
        manual.note_sent(&renderer, &queue, Some(21), vec![12, 13]);
        queue.queue_items.splice(3..3, [make(12, 22), make(13, 23)]);
        queue.version.minor += 1;
        manual.reconcile(&renderer, &queue);
        assert_eq!(manual.later_anchor(&renderer, &queue), Some(23));
        queue.queue_items.extend([make(30, 24), make(31, 25)]);
        queue.version.minor += 1;
        manual.reconcile(&renderer, &queue);
        assert_eq!(manual.later_anchor(&renderer, &queue), Some(23));
        queue.shuffle_mode = true;
        assert_eq!(manual.later_anchor(&renderer, &queue), Some(current));
        queue.shuffle_mode = false;
        queue.version.major += 1;
        manual.reconcile(&renderer, &queue);
        assert_eq!(manual.later_anchor(&renderer, &queue), Some(current));
    }

    #[test]
    fn upcoming_selection_preserves_distinct_duplicate_queue_item_ids() {
        let (queue, renderer) = remote_selection_fixture();
        let first = remote_upcoming_selection(&queue, &renderer, 0, 200).expect("first duplicate");
        let second =
            remote_upcoming_selection(&queue, &renderer, 1, 200).expect("second duplicate");
        assert_eq!(first.current_queue_item.unwrap().id, Some(11));
        assert_eq!(second.current_queue_item.unwrap().id, Some(12));
    }

    #[test]
    fn upcoming_selection_uses_ws_shuffle_and_only_sends_player_state() {
        let (mut queue, renderer) = remote_selection_fixture();
        queue.shuffle_mode = true;
        queue.shuffle_order = Some(vec![0, 3, 2, 1]);
        queue.autoplay_mode = true;
        let request = remote_upcoming_selection(&queue, &renderer, 0, 300).expect("shuffled row");
        assert_eq!(request.current_queue_item.as_ref().unwrap().id, Some(13));
        assert_eq!(request.playing_state, Some(super::PLAYING_STATE_PLAYING));
        assert_eq!(request.current_position, Some(0));
        let payload = serde_json::to_value(request).expect("player state payload");
        assert_eq!(payload.as_object().unwrap().len(), 3);
        for forbidden in [
            "track_ids",
            "shuffle_mode",
            "autoplay_reset",
            "context_uuid",
        ] {
            assert!(payload.get(forbidden).is_none(), "{forbidden}");
        }
    }

    #[test]
    fn upcoming_selection_refuses_changed_missing_or_incomplete_rows() {
        let (mut queue, renderer) = remote_selection_fixture();
        assert!(remote_upcoming_selection(&queue, &renderer, 0, 300).is_none());
        assert!(remote_upcoming_selection(&queue, &renderer, 100, 200).is_none());
        queue.shuffle_mode = true;
        assert!(remote_upcoming_selection(&queue, &renderer, 0, 200).is_none());
        queue.queue_items.clear();
        assert!(remote_upcoming_selection(&queue, &renderer, 0, 200).is_none());
    }

    #[test]
    fn upcoming_projection_validates_full_hydration_cursor_and_order() {
        let (mut queue, renderer) = remote_selection_fixture();
        queue.shuffle_mode = true;
        queue.shuffle_order = Some(vec![0, 3, 2, 1]);
        let (tracks, mut local) = local_selection_fixture(&queue);
        assert!(local_upcoming_matches_remote(
            &queue, &renderer, &tracks, &local
        ));
        let mut partial = tracks.clone();
        partial.remove(1);
        assert!(!local_upcoming_matches_remote(
            &queue, &renderer, &partial, &local
        ));
        local.upcoming.swap(0, 1);
        assert!(!local_upcoming_matches_remote(
            &queue, &renderer, &tracks, &local
        ));
        local.upcoming.swap(0, 1);
        local.current_index = Some(1);
        assert!(!local_upcoming_matches_remote(
            &queue, &renderer, &tracks, &local
        ));
    }

    #[test]
    fn upcoming_projection_rejects_foreign_sources_with_matching_catalog_ids() {
        let (queue, renderer) = remote_selection_fixture();
        let (mut tracks, local) = local_selection_fixture(&queue);
        for source in ["local", "plex", "jellyfin"] {
            tracks[1].source = Some(source.to_string());
            tracks[1].is_local = true;
            assert!(!local_upcoming_matches_remote(
                &queue, &renderer, &tracks, &local
            ));
        }
        tracks[1].source = Some("qobuz_download".to_string());
        assert!(local_upcoming_matches_remote(
            &queue, &renderer, &tracks, &local
        ));
    }

    #[test]
    fn peer_seek_uses_the_reported_track_not_the_old_local_track() {
        let mut old_local = track(Some("qobuz"), false);
        old_local.id = 10;
        old_local.duration_secs = 120;
        let mut peer = track(Some("qobuz_connect_remote"), false);
        peer.id = 20;
        peer.duration_secs = 360;
        let tracks = [old_local, peer];
        assert_eq!(peer_seek_position_ms(0.5, 20, &tracks), Some(180_000));
        assert_eq!(peer_seek_position_ms(0.5, 10, &tracks), Some(60_000));
        assert_eq!(peer_seek_position_ms(0.5, 30, &tracks), None);
        assert_eq!(peer_seek_position_ms(-1.0, 20, &tracks), Some(0));
        assert_eq!(peer_seek_position_ms(2.0, 20, &tracks), Some(360_000));
        assert_eq!(peer_seek_position_ms(f32::NAN, 20, &tracks), None);
    }

    #[test]
    fn peer_seek_refuses_unknown_or_unrepresentable_duration() {
        let mut row = track(Some("qobuz_connect_remote"), false);
        assert_eq!(peer_seek_position_ms(0.5, row.id, &[row.clone()]), None);
        row.duration_secs = u64::MAX;
        assert_eq!(peer_seek_position_ms(1.0, row.id, &[row]), None);
    }

    #[test]
    fn peer_seek_reanchors_the_peer_without_toggling_playback() {
        for playing_state in [2, 3] {
            let mut sync = peer_sync();
            ensure_session_renderer_state(&mut sync, 2).playing_state = Some(playing_state);
            assert!(project_peer_seek(&mut sync, 2, 0, 180_000, 200));
            let peer = &sync.session_renderer_states[&2];
            assert_eq!(peer.current_position_ms, Some(180_000));
            assert_eq!(peer.updated_at_ms, 200);
            assert_eq!(peer.current_queue_item_id, Some(0));
            assert_eq!(peer.playing_state, Some(playing_state));
        }
    }

    #[test]
    fn peer_seek_does_not_project_into_a_successor_track_or_renderer() {
        let mut sync = peer_sync();
        assert!(!project_peer_seek(&mut sync, 2, 9, 180_000, 200));
        sync.session.active_renderer_id = Some(3);
        assert!(!project_peer_seek(&mut sync, 2, 0, 180_000, 200));
        sync.session.active_renderer_id = Some(1);
        assert!(!project_peer_seek(&mut sync, 1, 0, 180_000, 200));
        assert_eq!(
            sync.session_renderer_states[&2].current_position_ms,
            Some(3_000)
        );
    }

    #[test]
    fn peer_mute_toggle_can_unmute_again_without_reanchoring_position() {
        let mut sync = peer_sync();
        for expected in [true, false, true] {
            let target = !sync.session_renderer_states[&2].muted.unwrap_or(false);
            assert_eq!(target, expected);
            assert!(project_peer_mute(&mut sync, 2, target));
            let peer = &sync.session_renderer_states[&2];
            assert_eq!(peer.muted, Some(expected));
            assert_eq!(peer.current_position_ms, Some(3_000));
            assert_eq!(peer.updated_at_ms, 100);
        }
    }

    #[test]
    fn peer_mute_does_not_project_into_a_successor_or_the_local_renderer() {
        let mut sync = peer_sync();
        sync.session.active_renderer_id = Some(3);
        assert!(!project_peer_mute(&mut sync, 2, true));
        sync.session.active_renderer_id = Some(1);
        assert!(!project_peer_mute(&mut sync, 1, true));
        assert_eq!(sync.session_renderer_states[&2].muted, Some(false));
        assert!(!sync.session_renderer_states.contains_key(&1));
    }

    #[test]
    fn qconnect_admits_catalog_and_offline_catalog_rows() {
        for source in [
            "qobuz",
            "qobuz_download",
            "qobuz_purchase",
            "offline",
            "qobuz_connect_remote",
        ] {
            assert!(
                is_qconnect_queue_track(&track(Some(source), true)),
                "{source}"
            );
        }
        assert!(is_qconnect_queue_track(&track(None, false)));
    }

    #[test]
    fn qconnect_rejects_every_server_and_file_source() {
        for source in ["local", "plex", "jellyfin", "subsonic", "navidrome"] {
            assert!(
                !is_qconnect_queue_track(&track(Some(source), true)),
                "{source}"
            );
        }
        assert!(!is_qconnect_queue_track(&track(None, true)));
    }

    #[test]
    fn qconnect_rejects_zero_even_when_stamped_qobuz() {
        let mut row = track(Some("qobuz"), false);
        row.id = 0;
        assert!(!is_qconnect_queue_track(&row));
    }

    #[test]
    fn controller_batches_keep_resolvable_rows_in_original_order() {
        let tracks = vec![
            (11, Some("qobuz".to_string())),
            (12, Some("local".to_string())),
            (13, Some("qobuz_connect_remote".to_string())),
            (14, Some("jellyfin".to_string())),
            (15, Some("qobuz_download".to_string())),
        ];
        let (kept, dropped) = resolvable_track_ids(&tracks);
        assert_eq!(kept, vec![11, 13, 15]);
        assert_eq!(dropped, 2);
    }

    #[test]
    fn defensive_queue_projection_remaps_a_dropped_cursor_forward() {
        let tracks = vec![
            track(Some("qobuz"), false),
            track(Some("local"), true),
            track(Some("qobuz_connect_remote"), false),
        ];
        let (kept, start, dropped) = resolvable_queue_projection(&tracks, Some(1));
        assert_eq!(kept, vec![42, 42]);
        assert_eq!(start, Some(1));
        assert_eq!(dropped, 1);
    }

    #[test]
    fn cast_teardown_replaces_only_the_exact_enabled_intent() {
        let source = include_str!("qconnect_qt.rs");
        let body = source
            .split_once("pub(crate) async fn disconnect_for_cast")
            .expect("Cast-specific QConnect teardown")
            .1
            .split_once("async fn disconnect_with_owner_policy_locked")
            .expect("Cast-specific teardown boundary")
            .0;

        let enabled_snapshot = body
            .find("enable_intent.current_token()")
            .expect("enabled-intent snapshot");
        let cast_revalidation = body
            .find("qconnect_start_intent_is_current(cast_epoch)")
            .expect("Cast epoch revalidation");
        let exact_disable = body
            .find("disable_if_current(expected)")
            .expect("exact enabled-intent disable");
        let lifecycle = body
            .find("lifecycle_gate.lock().await")
            .expect("lifecycle gate");
        let teardown = body
            .find("teardown_with_owner_policy_locked(true)")
            .expect("teardown without a second disable");
        let exact_revalidation = body
            .find("is_disabled_current(*token)")
            .expect("replacement token revalidation");

        assert!(enabled_snapshot < cast_revalidation);
        assert!(cast_revalidation < exact_disable);
        assert!(exact_disable < lifecycle);
        assert!(lifecycle < teardown);
        assert!(teardown < exact_revalidation);
        assert!(!body.contains(".disable()"));
    }

    #[test]
    fn manual_connect_publishes_cast_intent_before_renewing_enable() {
        let source = include_str!("qconnect_qt.rs");
        let body = source
            .split_once("pub async fn connect(&self)")
            .expect("manual QConnect entrypoint")
            .1
            .split_once("pub(crate) async fn connect_for_cast_restore")
            .expect("automatic restore entrypoint")
            .0;

        let cast_intent = body
            .find("begin_qconnect_start_intent()")
            .expect("synchronous Cast intent publication");
        let enabled_intent = body
            .find("enable_new_intent()")
            .expect("fresh manual enabled intent");
        let exact_acquire = body
            .find("acquire_exact_cast_transition(enable_token, cast_epoch)")
            .expect("exact Cast transition acquisition");

        assert!(cast_intent < enabled_intent);
        assert!(enabled_intent < exact_acquire);
    }
}
