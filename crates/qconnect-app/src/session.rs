//! Frontend-agnostic session/liveness primitives for Qobuz Connect.
//!
//! These are pure functions and pure data enums relocated out of the Tauri
//! adapter so that both the shipping Tauri adapter and a future Slint adapter
//! can share the exact same session-arbitration / liveness logic. Behavior is
//! byte-for-byte identical to the prior Tauri-side definitions.

use qbz_models::Quality;
use qconnect_core::{QConnectQueueState, QConnectRendererState, QueueItem};
use serde::{Deserialize, Serialize};

use crate::queue_resolution::{
    find_cursor_index_by_queue_item_id, normalize_current_queue_item_id_from_queue_state,
    ordered_queue_cursors, QconnectOrderedQueueCursor,
};

/// Lifecycle state surfaced to the UI so the toggle can reflect what the user
/// asked for (`running`) separately from what the transport currently has
/// (`Connecting`/`Connected`/`Reconnecting`). Without this distinction the UI
/// reads the toggle as "off" while the backend reconnect loop is alive,
/// leaving the user unable to disable QConnect (issue #358).
///
/// Pure data — no Tauri dependency. The Tauri `types.rs` re-exports this for the
/// command surface; the session layer emits it via `QconnectAppEvent::LifecycleChanged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QconnectLifecycleState {
    /// User has not enabled QConnect (or it has been fully torn down).
    #[default]
    Off,
    /// `connect()` has been called; transport is establishing the WS but no
    /// `SessionEstablished` yet.
    Connecting,
    /// Transport saw at least one `SESSION_STATE` frame on the active WS — the
    /// session-level handshake completed.
    Connected,
    /// Transport disconnected after at least one successful connect; the
    /// reconnect loop is running.
    Reconnecting,
    /// `MaxReconnectAttemptsExceeded` fired — runtime auto-stopped, last error
    /// surfaced. User can re-enable from UI.
    Exhausted,
}

/// Playing-state wire value for PLAYING. Mirrors the Tauri adapter's
/// `PLAYING_STATE_PLAYING` (the renderer reports playing_state == 2 while
/// actively playing). `pub(crate)` so the relocated session-apply/watchdog
/// logic in `app.rs` reads the same constant.
pub(crate) const PLAYING_STATE_PLAYING: i32 = 2;

/// Playing-state wire value for UNKNOWN (== 0). The freeze path stamps a dead
/// renderer's cached `playing_state` to UNKNOWN so the projection stops lying.
/// Mirrors the Tauri adapter's `PLAYING_STATE_UNKNOWN`.
pub(crate) const PLAYING_STATE_UNKNOWN: i32 = 0;

/// JoinSession `reason` wire values (proto tag 3): a first join from a fresh
/// runtime is a controller request, a join after a transport drop carries the
/// reconnection reason so the server treats it as session continuity rather
/// than a brand-new controller (P1-2).
pub const JOIN_SESSION_REASON_CONTROLLER_REQUEST: i32 = 1;
pub const JOIN_SESSION_REASON_RECONNECTION: i32 = 2;

/// Official "renderer LOST" silence budget. A *playing* active peer renderer
/// that sends no RENDERER_STATE_UPDATED for this long is considered
/// unreachable (webplayer arms setTimeout(...,12e3) on onPlayerStateUpdated
/// while playingState==PLAY). See 05-sync-status-queue.md §1.
pub const QCONNECT_RENDERER_LOST_TIMEOUT_MS: u64 = 12_000;

/// Convert player/catalog seconds to the QConnect wire unit exactly once.
pub const fn qconnect_millis_from_secs(seconds: u64) -> u64 {
    seconds.saturating_mul(1_000)
}

/// Pure arming predicate for the renderer-liveness watchdog: arm only while the
/// active renderer is a peer AND its reported playing_state is PLAYING.
pub fn should_arm_renderer_watchdog(playing_state: Option<i32>, is_active_peer: bool) -> bool {
    is_active_peer && playing_state == Some(PLAYING_STATE_PLAYING)
}

/// The server's view of who currently owns the active-renderer slot in a
/// SESSION_STATE frame, classified relative to us (P1-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerActiveState {
    /// No renderer is active in the session.
    None,
    /// We are the active renderer.
    Me,
    /// Another renderer is active and reports PLAYING.
    OtherPlaying,
    /// Another renderer is active and reports a non-playing state.
    OtherPaused,
}

/// Outcome of takeover arbitration on a SESSION_STATE frame (P1-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionDecision {
    /// We should consider ourselves the active renderer.
    pub should_be_active: bool,
    /// We should emit CtrlSrvrSetActiveRenderer to claim the slot.
    pub should_set_active_renderer: bool,
    /// We should push our queue + state to the session.
    pub should_set_queue: bool,
    /// We should ask the renderer for its current state.
    pub should_ask_queue: bool,
}

/// Pure takeover arbitration ported from the web controller's
/// `computeConnectionState` and completed with QBZ's local-playback rule. A
/// playing peer remains authoritative. When QBZ itself is already playing and
/// the active peer is paused/stopped (or the session is idle), QBZ keeps
/// playback, claims the renderer slot, and publishes its resolvable queue.
/// Repeat-mode reconciliation remains outside this function; it overlaps the
/// existing `session_loop_mode` handling in the event sinks.
pub fn compute_connection_state(
    was_active: bool,
    was_playing: bool,
    server: ServerActiveState,
    queue_equal: bool,
    auto_take_when_idle: bool,
) -> ConnectionDecision {
    use ServerActiveState::*;
    match server {
        None => ConnectionDecision {
            // Auto-take the render when the cloud reports NO active renderer and
            // `auto_take_when_idle` is set (the caller gates this to a FRESH
            // connect — see run_session_loop), so the user does not need an extra
            // "Play here" click when QBZ is the only candidate. This NEVER steals
            // from an active peer: an active peer is server==Other*/Me, not None,
            // and those branches keep should_set_active_renderer=false.
            should_be_active: was_active || was_playing || auto_take_when_idle,
            should_set_active_renderer: was_active || was_playing || auto_take_when_idle,
            should_set_queue: was_active || was_playing,
            should_ask_queue: false,
        },
        Me => ConnectionDecision {
            should_be_active: true,
            should_set_active_renderer: false,
            should_set_queue: !queue_equal && (was_active || was_playing),
            should_ask_queue: queue_equal,
        },
        OtherPlaying => ConnectionDecision {
            should_be_active: false,
            should_set_active_renderer: false,
            should_set_queue: false,
            should_ask_queue: true,
        },
        OtherPaused if was_playing => ConnectionDecision {
            should_be_active: true,
            should_set_active_renderer: true,
            should_set_queue: true,
            should_ask_queue: false,
        },
        OtherPaused => ConnectionDecision {
            should_be_active: false,
            should_set_active_renderer: false,
            should_set_queue: false,
            should_ask_queue: true,
        },
    }
}

/// Pure selector for the JoinSession `reason`: a post-drop rejoin carries
/// RECONNECTION, the first join from a fresh runtime carries CONTROLLER_REQUEST
/// (P1-2).
pub fn deferred_join_reason(has_disconnected: bool) -> i32 {
    if has_disconnected {
        JOIN_SESSION_REASON_RECONNECTION
    } else {
        JOIN_SESSION_REASON_CONTROLLER_REQUEST
    }
}

/// Pure predicate (P1-8): keep re-asking for queue state after a Lagged
/// broadcast drop until the session_uuid is confirmed or the attempt budget is
/// spent. Stops immediately once the session_uuid is known.
pub fn should_reask_queue_state(
    session_uuid_known: bool,
    attempts: u32,
    max_attempts: u32,
) -> bool {
    !session_uuid_known && attempts < max_attempts
}

/// Wire renderer status from `MESSAGE_TYPE_SRVR_CTRL_RENDERER_STATE_UPDATED`
/// (`status` field, decoded at qconnect-protocol decoder.rs:801).
/// Wire enum: UNKNOWN=0, ACTIVE_CONNECTED=1, ACTIVE_DISCONNECTED=2, INACTIVE=3.
/// Per the official client's `Ya` collapse, UNKNOWN and any UNRECOGNIZED value
/// map to INACTIVE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RendererStatus {
    ActiveConnected,
    ActiveDisconnected,
    Inactive,
}

impl RendererStatus {
    pub fn from_wire(value: Option<i64>) -> Self {
        match value {
            Some(1) => Self::ActiveConnected,
            Some(2) => Self::ActiveDisconnected,
            // 0 (UNKNOWN), 3 (INACTIVE), any UNRECOGNIZED value, and a missing
            // field all collapse to INACTIVE.
            _ => Self::Inactive,
        }
    }
}

/// Map a QConnect `max_audio_quality` level to a qbz `Quality`.
/// QConnect levels: 0/1 ~ MP3, 2 ~ CD/Lossless, 3 ~ Hi-Res (<=96kHz),
/// 4 ~ Hi-Res (>96kHz), 5/None ~ uncapped. The qbz `Quality` enum only has
/// four variants (Mp3, Lossless, HiRes, UltraHiRes), so 4 and uncapped both
/// resolve to UltraHiRes.
pub fn quality_from_max_audio_quality(level: Option<i32>) -> Quality {
    match level {
        Some(l) if l <= 1 => Quality::Mp3,
        Some(2) => Quality::Lossless,
        Some(3) => Quality::HiRes,
        Some(4) => Quality::UltraHiRes,
        _ => Quality::UltraHiRes,
    }
}

/// Map the local quality cap to the exact QConnect `max_audio_quality` wire
/// level. Join reports from every host adapter must use this one mapping.
pub const fn max_audio_quality_from_quality(quality: Quality) -> i32 {
    match quality {
        Quality::Mp3 => 1,
        Quality::Lossless => 2,
        Quality::HiRes => 3,
        Quality::UltraHiRes => 4,
    }
}

// ---------------------------------------------------------------------------
// Session topology types (relocated from the Tauri adapter — slice 2+4).
//
// These are pure data describing who is in a QConnect session and the cloud's
// per-renderer view. They move here (frontend-agnostic) so both the Tauri and
// Slint adapters share one session-topology model under a single lock. Field
// visibility is widened to `pub` because the adapters access these fields
// across the crate boundary; serialized shape is unchanged.
// ---------------------------------------------------------------------------

/// Per-renderer cached state derived from session-management events
/// (RENDERER_STATE_UPDATED etc.): the cloud's latest view of one renderer.
#[derive(Debug, Clone, Default)]
pub struct QconnectSessionRendererState {
    pub active: Option<bool>,
    pub playing_state: Option<i32>,
    pub current_position_ms: Option<u64>,
    pub current_queue_item_id: Option<u64>,
    pub volume: Option<i32>,
    pub muted: Option<bool>,
    pub max_audio_quality: Option<i32>,
    pub loop_mode: Option<i32>,
    pub shuffle_mode: Option<bool>,
    pub updated_at_ms: u64,
}

/// Session topology — stored from session management events (types 81-87):
/// who is in the session, who is active, and which renderer is us.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QconnectSessionState {
    pub session_uuid: Option<String>,
    pub active_renderer_id: Option<i32>,
    pub local_renderer_id: Option<i32>,
    pub renderers: Vec<QconnectRendererInfo>,
}

/// One renderer registered in the QConnect session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QconnectRendererInfo {
    pub renderer_id: i32,
    pub device_uuid: Option<String>,
    pub friendly_name: Option<String>,
    pub brand: Option<String>,
    pub model: Option<String>,
    pub device_type: Option<i32>,
    /// capabilities.volume_remote_control. 1 == NOT_ALLOWED, 2 == ALLOWED. None == not advertised
    /// (treated as allowed to avoid regressing renderers that omit it).
    #[serde(default)]
    pub volume_remote_control: Option<i32>,
}

/// Resolved local-device identity injected by the frontend adapter into the
/// session layer. Lets `refresh_local_renderer_id` stay frontend-agnostic: the
/// Tauri adapter resolves it from its persisted device uuid + device-info
/// builder, the Slint adapter from the shared qbz-app identity — qconnect-app
/// never depends on the persistence/transport crates.
#[derive(Debug, Clone, Default)]
pub struct LocalIdentity {
    pub device_uuid: String,
    pub friendly_name: Option<String>,
    pub brand: Option<String>,
    pub model: Option<String>,
    pub device_type: Option<i32>,
}

/// Snapshot of the last reported file (stream) audio quality, used to dedup
/// outbound RndrSrvrFileAudioQualityChanged reports. Moved here with the
/// session topology so the relocated sync accumulator is self-contained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QconnectFileAudioQualitySnapshot {
    pub sampling_rate: i32,
    pub bit_depth: i32,
    pub nb_channels: i32,
    pub audio_quality: i32,
}

/// Whether the active renderer permits remote volume control. Absent capability
/// (None) defaults to allowed; only an explicit non-ALLOWED value disables.
pub fn renderer_allows_remote_volume(info: &QconnectRendererInfo) -> bool {
    matches!(info.volume_remote_control, None | Some(2))
}

/// Resolve the local renderer id within the session by matching the injected
/// local identity: first by exact device_uuid, then by a unique device
/// fingerprint (name+brand+model+type), then a looser name+type fingerprint.
/// Identical resolution order to the prior Tauri-side fn; identity is now data.
pub fn refresh_local_renderer_id(session: &mut QconnectSessionState, identity: &LocalIdentity) {
    if let Some(renderer_id) = session
        .renderers
        .iter()
        .find(|renderer| renderer.device_uuid.as_deref() == Some(identity.device_uuid.as_str()))
        .map(|renderer| renderer.renderer_id)
    {
        session.local_renderer_id = Some(renderer_id);
        return;
    }

    let local_friendly_name = identity.friendly_name.as_deref();
    let local_brand = identity.brand.as_deref();
    let local_model = identity.model.as_deref();
    let local_device_type = identity.device_type;

    // Some server ADD_RENDERER payloads omit device_uuid for the local renderer.
    // Fall back to a unique device fingerprint so controller-side handoff logic
    // can still distinguish local vs peer renderers.
    if let Some(renderer_id) = find_unique_renderer_id(session, |renderer| {
        renderer.friendly_name.as_deref() == local_friendly_name
            && renderer.brand.as_deref() == local_brand
            && renderer.model.as_deref() == local_model
            && renderer.device_type == local_device_type
    }) {
        session.local_renderer_id = Some(renderer_id);
        return;
    }

    if let Some(renderer_id) = find_unique_renderer_id(session, |renderer| {
        renderer.friendly_name.as_deref() == local_friendly_name
            && renderer.device_type == local_device_type
    }) {
        session.local_renderer_id = Some(renderer_id);
        return;
    }

    session.local_renderer_id = None;
}

pub fn normalize_active_renderer_id(value: Option<i64>) -> Option<i32> {
    value
        .filter(|renderer_id| *renderer_id >= 0)
        .and_then(|renderer_id| i32::try_from(renderer_id).ok())
}

pub fn is_peer_renderer_active(session: &QconnectSessionState) -> bool {
    match (session.active_renderer_id, session.local_renderer_id) {
        (Some(active_renderer_id), Some(local_renderer_id)) => {
            active_renderer_id != local_renderer_id
        }
        _ => false,
    }
}

pub fn is_local_renderer_active(session: &QconnectSessionState) -> bool {
    match (session.active_renderer_id, session.local_renderer_id) {
        (Some(active_renderer_id), Some(local_renderer_id)) => {
            active_renderer_id == local_renderer_id
        }
        _ => false,
    }
}

pub fn find_unique_renderer_id(
    session: &QconnectSessionState,
    predicate: impl Fn(&QconnectRendererInfo) -> bool,
) -> Option<i32> {
    let mut matches = session
        .renderers
        .iter()
        .filter(|renderer| predicate(renderer))
        .map(|renderer| renderer.renderer_id);

    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }

    Some(first)
}

// ---------------------------------------------------------------------------
// Session-projection helpers (relocated from the Tauri adapter — slice 6, Slint
// port). Pure functions over the cloud queue + cached per-renderer state. They
// move here (frontend-agnostic) so the Tauri event sink AND the Slint event sink
// build the same renderer projection from one definition. Byte-for-byte
// identical to the prior Tauri-side bodies.
// ---------------------------------------------------------------------------

/// Snapshot the queue item a cursor points at, normalizing the queue_item_id for
/// in-queue cursors (autoplay items carry their own id verbatim).
pub fn queue_item_snapshot_for_cursor(
    queue: &QConnectQueueState,
    cursor: QconnectOrderedQueueCursor,
) -> Option<QueueItem> {
    match cursor {
        QconnectOrderedQueueCursor::Queue(index) => {
            queue.queue_items.get(index).cloned().map(|mut item| {
                item.queue_item_id = normalize_current_queue_item_id_from_queue_state(queue, index);
                item
            })
        }
        QconnectOrderedQueueCursor::Autoplay(index) => queue.autoplay_items.get(index).cloned(),
    }
}

/// Build a renderer-state projection from the cloud queue + a cached per-renderer
/// state, resolving the current/next track by the renderer's current_queue_item_id
/// against the ordered cursors. `session_loop_mode` is the fallback loop mode when
/// the renderer state omits one.
pub fn build_session_renderer_snapshot(
    queue: &QConnectQueueState,
    renderer_state: Option<&QconnectSessionRendererState>,
    session_loop_mode: Option<i32>,
) -> QConnectRendererState {
    let renderer_state = renderer_state.cloned().unwrap_or_default();
    let cursors = ordered_queue_cursors(queue);
    let current_index =
        find_cursor_index_by_queue_item_id(&cursors, queue, renderer_state.current_queue_item_id);
    let current_track =
        current_index.and_then(|index| queue_item_snapshot_for_cursor(queue, cursors[index]));
    let next_track = current_index
        .and_then(|index| cursors.get(index + 1).copied())
        .and_then(|cursor| queue_item_snapshot_for_cursor(queue, cursor));

    QConnectRendererState {
        active: renderer_state.active,
        playing_state: renderer_state.playing_state,
        current_position_ms: renderer_state.current_position_ms,
        current_track,
        next_track,
        volume: renderer_state.volume,
        volume_delta: None,
        muted: renderer_state.muted,
        max_audio_quality: renderer_state.max_audio_quality,
        loop_mode: renderer_state.loop_mode.or(session_loop_mode),
        shuffle_mode: renderer_state.shuffle_mode,
        updated_at_ms: renderer_state.updated_at_ms,
    }
}

/// Merge a base renderer snapshot (the local player's view) with the cloud's
/// cached per-renderer state and the session loop mode, producing the
/// "effective" renderer state the controller should reason about. Pure: no
/// frontend/Tauri dependency. Hoisted from the Tauri adapter (ADR-006) so the
/// Tauri and Slint controller paths share one definition.
pub fn build_effective_renderer_snapshot(
    queue: &QConnectQueueState,
    base_renderer_state: &QConnectRendererState,
    session_renderer_state: Option<&QconnectSessionRendererState>,
    session_loop_mode: Option<i32>,
) -> QConnectRendererState {
    let mut renderer_snapshot = base_renderer_state.clone();

    if let Some(session_renderer_state) = session_renderer_state {
        if let Some(active) = session_renderer_state.active {
            renderer_snapshot.active = Some(active);
        }
        if let Some(playing_state) = session_renderer_state.playing_state {
            renderer_snapshot.playing_state = Some(playing_state);
        }
        if let Some(current_position_ms) = session_renderer_state.current_position_ms {
            renderer_snapshot.current_position_ms = Some(current_position_ms);
        }
        if let Some(volume) = session_renderer_state.volume {
            renderer_snapshot.volume = Some(volume);
        }
        if let Some(muted) = session_renderer_state.muted {
            renderer_snapshot.muted = Some(muted);
        }
        if let Some(max_audio_quality) = session_renderer_state.max_audio_quality {
            renderer_snapshot.max_audio_quality = Some(max_audio_quality);
        }
        if let Some(loop_mode) = session_renderer_state.loop_mode.or(session_loop_mode) {
            renderer_snapshot.loop_mode = Some(loop_mode);
        }
        if let Some(shuffle_mode) = session_renderer_state.shuffle_mode {
            renderer_snapshot.shuffle_mode = Some(shuffle_mode);
        }
        if session_renderer_state.updated_at_ms > 0 {
            renderer_snapshot.updated_at_ms = session_renderer_state.updated_at_ms;
        }

        if session_renderer_state.current_queue_item_id.is_some() {
            let session_snapshot = build_session_renderer_snapshot(
                queue,
                Some(session_renderer_state),
                session_loop_mode,
            );
            if session_snapshot.current_track.is_some() {
                renderer_snapshot.current_track = session_snapshot.current_track;
                renderer_snapshot.next_track = session_snapshot.next_track;
            }
        }
    } else if let Some(loop_mode) = session_loop_mode {
        renderer_snapshot.loop_mode = Some(loop_mode);
    }

    renderer_snapshot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_volume_capability_matches_official_wire_values() {
        for (wire, allowed) in [
            (None, true),
            (Some(0), false),
            (Some(1), false),
            (Some(2), true),
            (Some(3), false),
        ] {
            let info: QconnectRendererInfo = serde_json::from_value(serde_json::json!({
                "renderer_id": 7, "volume_remote_control": wire
            }))
            .unwrap();
            assert_eq!(
                renderer_allows_remote_volume(&info),
                allowed,
                "wire={wire:?}"
            );
        }
    }

    #[test]
    fn seconds_convert_to_qconnect_milliseconds_once() {
        assert_eq!(qconnect_millis_from_secs(317), 317_000);
        assert_eq!(qconnect_millis_from_secs(u64::MAX), u64::MAX);
    }

    #[test]
    fn deferred_join_reason_is_reconnection_only_after_a_drop() {
        assert_eq!(
            deferred_join_reason(false),
            JOIN_SESSION_REASON_CONTROLLER_REQUEST
        );
        assert_eq!(deferred_join_reason(true), JOIN_SESSION_REASON_RECONNECTION);
    }

    #[test]
    fn reask_queue_state_stops_once_session_uuid_known_or_budget_spent() {
        assert!(should_reask_queue_state(false, 0, 5));
        assert!(should_reask_queue_state(false, 4, 5));
        assert!(!should_reask_queue_state(false, 5, 5));
        assert!(!should_reask_queue_state(true, 0, 5));
    }

    #[test]
    fn compute_connection_state_matrix() {
        use ServerActiveState::*;
        let d = compute_connection_state(true, true, None, false, false);
        assert!(
            d.should_be_active
                && d.should_set_active_renderer
                && d.should_set_queue
                && !d.should_ask_queue
        );
        let d = compute_connection_state(true, false, Me, false, false);
        assert!(d.should_be_active && !d.should_set_active_renderer && d.should_set_queue);
        let d = compute_connection_state(false, false, Me, true, false);
        assert!(d.should_ask_queue && !d.should_set_queue && !d.should_set_active_renderer);
        let d = compute_connection_state(false, false, OtherPlaying, false, false);
        assert!(!d.should_be_active && !d.should_set_active_renderer && d.should_ask_queue);
        // A paused/stopped peer does not interrupt real local playback: QBZ
        // claims the slot and publishes its filtered queue.
        let d = compute_connection_state(false, true, OtherPaused, false, false);
        assert!(d.should_be_active && d.should_set_active_renderer && d.should_set_queue);
        // A genuinely playing peer remains authoritative.
        let d = compute_connection_state(false, true, OtherPlaying, false, false);
        assert!(!d.should_be_active && !d.should_set_active_renderer && !d.should_set_queue);
        let d = compute_connection_state(false, false, None, false, false);
        assert!(
            !d.should_be_active
                && !d.should_set_active_renderer
                && !d.should_set_queue
                && !d.should_ask_queue
        );
        // Fresh connect, cloud reports nobody active, auto_take_when_idle set ->
        // QBZ claims the render (no extra "Play here" click).
        let d = compute_connection_state(false, false, None, false, true);
        assert!(d.should_be_active && d.should_set_active_renderer);
        // Local playback itself is sufficient to claim an idle session and
        // upload its queue, even without the generic idle auto-take flag.
        let d = compute_connection_state(false, true, None, false, false);
        assert!(d.should_be_active && d.should_set_active_renderer && d.should_set_queue);
        // An active peer is server==OtherPlaying/Other Paused, never None, so
        // auto_take_when_idle can never steal it (guard at the call site too).
        let d = compute_connection_state(false, false, OtherPlaying, false, true);
        assert!(!d.should_set_active_renderer);
    }

    #[test]
    fn renderer_status_from_wire_maps_known_values() {
        assert_eq!(RendererStatus::from_wire(Some(0)), RendererStatus::Inactive); // UNKNOWN collapses
        assert_eq!(
            RendererStatus::from_wire(Some(1)),
            RendererStatus::ActiveConnected
        );
        assert_eq!(
            RendererStatus::from_wire(Some(2)),
            RendererStatus::ActiveDisconnected
        );
        assert_eq!(RendererStatus::from_wire(Some(3)), RendererStatus::Inactive);
    }

    #[test]
    fn renderer_status_from_wire_collapses_unknown_and_missing_to_inactive() {
        assert_eq!(
            RendererStatus::from_wire(Some(99)),
            RendererStatus::Inactive
        ); // UNRECOGNIZED
        assert_eq!(RendererStatus::from_wire(None), RendererStatus::Inactive); // absent field
    }

    #[test]
    fn watchdog_arms_only_for_playing_active_peer() {
        const PLAYING_STATE_UNKNOWN: i32 = 0;
        const PLAYING_STATE_STOPPED: i32 = 1;
        const PLAYING_STATE_PAUSED: i32 = 3;
        // Arm: playing AND active peer.
        assert!(should_arm_renderer_watchdog(
            Some(PLAYING_STATE_PLAYING),
            true
        ));
        // Do not arm when paused/stopped/unknown even if active peer.
        assert!(!should_arm_renderer_watchdog(
            Some(PLAYING_STATE_PAUSED),
            true
        ));
        assert!(!should_arm_renderer_watchdog(
            Some(PLAYING_STATE_STOPPED),
            true
        ));
        assert!(!should_arm_renderer_watchdog(
            Some(PLAYING_STATE_UNKNOWN),
            true
        ));
        assert!(!should_arm_renderer_watchdog(None, true));
        // Do not arm when not an active peer (e.g. local renderer is active).
        assert!(!should_arm_renderer_watchdog(
            Some(PLAYING_STATE_PLAYING),
            false
        ));
    }

    #[test]
    fn quality_from_max_audio_quality_maps_levels() {
        // qbz Quality has four variants: Mp3, Lossless (CD), HiRes (<=96kHz),
        // UltraHiRes (>96kHz). QConnect levels collapse onto these.
        assert_eq!(quality_from_max_audio_quality(Some(0)), Quality::Mp3);
        assert_eq!(quality_from_max_audio_quality(Some(1)), Quality::Mp3);
        assert_eq!(quality_from_max_audio_quality(Some(2)), Quality::Lossless);
        assert_eq!(quality_from_max_audio_quality(Some(3)), Quality::HiRes);
        assert_eq!(quality_from_max_audio_quality(Some(4)), Quality::UltraHiRes);
        assert_eq!(quality_from_max_audio_quality(Some(5)), Quality::UltraHiRes);
        assert_eq!(quality_from_max_audio_quality(None), Quality::UltraHiRes);
        assert_eq!(
            quality_from_max_audio_quality(Some(99)),
            Quality::UltraHiRes
        );
    }

    #[test]
    fn local_quality_maps_to_exact_qconnect_wire_levels() {
        assert_eq!(max_audio_quality_from_quality(Quality::Mp3), 1);
        assert_eq!(max_audio_quality_from_quality(Quality::Lossless), 2);
        assert_eq!(max_audio_quality_from_quality(Quality::HiRes), 3);
        assert_eq!(max_audio_quality_from_quality(Quality::UltraHiRes), 4);
    }
    #[test]
    fn uuid_disambiguates_identical_renderer_names() {
        let mut session = super::QconnectSessionState::default();
        session.renderers = [1, 2].into_iter().map(|id| super::QconnectRendererInfo {
            renderer_id: id,
            device_uuid: Some(format!("uuid-{id}")),
            friendly_name: Some("QBZ".into()),
            brand: None, model: None, device_type: None, volume_remote_control: None,
        }).collect();
        let identity = super::LocalIdentity {
            device_uuid: "uuid-2".into(),
            friendly_name: Some("QBZ".into()),
            ..Default::default()
        };
        super::refresh_local_renderer_id(&mut session, &identity);
        assert_eq!(session.local_renderer_id, Some(2));
        for renderer in &mut session.renderers { renderer.device_uuid = None; }
        super::refresh_local_renderer_id(&mut session, &identity);
        assert_eq!(session.local_renderer_id, None);
        session.renderers.remove(0);
        super::refresh_local_renderer_id(&mut session, &identity);
        assert_eq!(session.local_renderer_id, Some(2));
    }

}
