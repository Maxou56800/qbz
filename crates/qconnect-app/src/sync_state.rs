//! Cross-frontend QConnect remote-sync accumulator.
//!
//! Caches the cloud's renderer/queue snapshots, the most recent
//! materialization, the session topology, the per-renderer cached state, the
//! load-attempt dedup window, and the renderer-liveness watchdog epoch.
//!
//! This is held behind a SINGLE Mutex shared by the session/liveness path and
//! the renderer-materialize path. That single-lock sharing is load-bearing:
//! `capture_session_state_takeover_input` reads session topology together with
//! per-renderer state, and the materialize path reads `last_renderer_*` together
//! with `last_applied_queue_state`, all atomically. Splitting these across two
//! locks would tear the takeover decision. So it stays ONE struct under ONE
//! lock. Relocated here (slice 2+4) so all adapters share it; the owning `Mutex`
//! is held by the adapter / `QconnectApp`.
//!
//! Mutated by the event sink (on inbound events), the renderer engine (on
//! materialize/apply), track loading (on load attempt), and the service loop
//! (on outbound report completions).

use std::collections::HashMap;
use std::time::Instant;

use qconnect_core::{QConnectQueueState, QConnectRendererState};

use crate::session::{
    QconnectFileAudioQualitySnapshot, QconnectSessionRendererState, QconnectSessionState,
};

#[derive(Debug)]
pub struct PendingLocalQueueTakeover {
    expected_track_ids: Vec<u64>,
    action_uuid: String,
    action_confirmed: bool,
    retry_needed: bool,
}

#[derive(Debug, Default)]
pub struct QconnectRemoteSyncState {
    /// Invalidates renderer transport intent when session ownership is lost.
    pub renderer_generation: u64,
    /// Explicit occurrence received before its transport state was known.
    pub pending_transport_target: Option<(u64, qconnect_core::QueueItem)>,
    pub last_renderer_queue_item_id: Option<u64>,
    pub last_renderer_next_queue_item_id: Option<u64>,
    pub last_renderer_track_id: Option<u64>,
    pub last_renderer_next_track_id: Option<u64>,
    pub last_renderer_playing_state: Option<i32>,
    pub last_materialized_start_index: Option<usize>,
    pub last_materialized_core_shuffle_order: Option<Vec<usize>>,
    pub last_reported_file_audio_quality: Option<QconnectFileAudioQualitySnapshot>,
    /// Last reported device (DAC output) audio quality: (sampling_rate, bit_depth, nb_channels).
    /// Used to dedup outbound RndrSrvrDeviceAudioQualityChanged(27) reports.
    pub last_reported_device_audio_quality: Option<(i32, i32, i32)>,
    pub last_applied_queue_state: Option<QConnectQueueState>,
    pub last_remote_queue_state: Option<QConnectQueueState>,
    /// Queue uploaded because QBZ was already playing while no peer was. A
    /// stale bootstrap queue reply must not overwrite local playback before the
    /// queue-load echo lands.
    pub pending_local_queue_takeover: Option<PendingLocalQueueTakeover>,
    /// The exact local QueueLoadTracks echo landed, but QBZ has not yet
    /// published its real playing position/state against that queue version.
    /// Keep renderer commands fenced through that final assertion so the
    /// server cannot seek/pause local audio with an older cursor in between.
    pub local_playback_state_assertion_pending: bool,
    /// A SESSION_STATE reported a peer renderer while local audio was already
    /// playing. The frontend is asking which queue/renderer should win. While
    /// this is true, no remote queue or renderer command may mutate local audio.
    pub local_playback_conflict_pending: bool,
    pub session_loop_mode: Option<i32>,
    /// Session topology — stored from session management events (types 81-87).
    pub session: QconnectSessionState,
    /// The session_uuid for which we last ran the full deferred renderer-join
    /// body. Used to make the deferred join idempotent (P1-8): when a SESSION_STATE
    /// arrives with the same session_uuid we skip the join reports but still
    /// re-AskForRendererState.
    pub last_joined_session_uuid: Option<String>,
    pub session_renderer_states: HashMap<i32, QconnectSessionRendererState>,
    /// Track of the most recent load attempt across paths (V2 play handoff and
    /// ensure_remote_track_loaded). Used to suppress redundant reloads when an
    /// echo SetState arrives during the in-progress buffer/decode window of a
    /// previously triggered load.
    pub last_load_attempt: Option<(u64, Instant)>,
    /// Monotonic epoch for the renderer-liveness watchdog (P0-1). Every armed
    /// RENDERER_STATE_UPDATED bumps this; a spawned 12s task captures the value
    /// and no-ops on wake if it was superseded (reset/disarm). Disarm =
    /// pause/stop/active-change/disconnect, which also bump it.
    pub watchdog_generation: u64,
}

/// Arm local queue authority after a takeover upload is accepted by the
/// transport. It remains armed until the matching cloud echo arrives, a peer
/// explicitly wins, or the runtime disconnects. A wall-clock timeout is unsafe:
/// a slow or rejected queue load can arrive after it and overwrite live audio.
pub fn arm_local_queue_takeover(
    state: &mut QconnectRemoteSyncState,
    track_ids: Vec<u64>,
    action_uuid: String,
) {
    if track_ids.is_empty() {
        state.pending_local_queue_takeover = None;
        return;
    }
    state.pending_local_queue_takeover = Some(PendingLocalQueueTakeover {
        expected_track_ids: track_ids,
        action_uuid,
        action_confirmed: false,
        retry_needed: false,
    });
    state.local_playback_state_assertion_pending = false;
}

/// Mark the exact QueueLoadTracks action that established local authority as
/// accepted. Track ids alone are not an acknowledgement: a stale bootstrap
/// queue can contain the same album while carrying an older cursor/position.
pub fn confirm_local_queue_takeover_action(
    state: &mut QconnectRemoteSyncState,
    action_uuid: &str,
) -> bool {
    let Some(pending) = state.pending_local_queue_takeover.as_mut() else {
        return false;
    };
    if pending.action_uuid != action_uuid {
        return false;
    }
    pending.action_confirmed = true;
    true
}

/// Keep local authority fenced after the matching QueueLoadTracks action was
/// rejected, canceled by concurrency, or timed out. The next sync uses the
/// refreshed queue version and replaces this action uuid.
pub fn reject_local_queue_takeover_action(
    state: &mut QconnectRemoteSyncState,
    action_uuid: &str,
) -> bool {
    let Some(pending) = state.pending_local_queue_takeover.as_mut() else {
        return false;
    };
    if pending.action_uuid != action_uuid {
        return false;
    }
    pending.action_confirmed = false;
    pending.retry_needed = true;
    true
}

/// True when a rejected/mismatching cloud queue requires the local projection
/// to be sent again even if the source queue itself has not changed.
pub fn local_queue_takeover_needs_retry(state: &QconnectRemoteSyncState) -> bool {
    state
        .pending_local_queue_takeover
        .as_ref()
        .map(|pending| pending.retry_needed)
        .unwrap_or(false)
}

/// Renderer commands carry the cloud's prior cursor independently of queue
/// snapshots. Fence them alongside queue materialization during an unresolved
/// conflict or a local-queue takeover, otherwise SetActive/SetState can create
/// a stale single-track queue before the matching queue echo arrives.
pub fn remote_renderer_commands_are_fenced(state: &QconnectRemoteSyncState) -> bool {
    state.local_playback_conflict_pending
        || state.pending_local_queue_takeover.is_some()
        || state.local_playback_state_assertion_pending
}

/// Release the final renderer-command fence only after QBZ has successfully
/// reported the actual local position/state for the accepted queue version.
pub fn confirm_local_playback_state_asserted(state: &mut QconnectRemoteSyncState) {
    state.local_playback_state_assertion_pending = false;
}

/// Fence renderer/queue side effects while the frontend resolves a playback
/// conflict. This is separate from queue takeover authority because choices 1
/// and 2 deliberately let the remote queue win.
pub fn set_local_playback_conflict_pending(state: &mut QconnectRemoteSyncState, pending: bool) {
    state.local_playback_conflict_pending = pending;
}

/// Clear takeover protection on disconnect/retirement.
pub fn clear_local_queue_takeover(state: &mut QconnectRemoteSyncState) {
    state.renderer_generation = state.renderer_generation.wrapping_add(1);
    state.last_load_attempt = None;
    state.pending_local_queue_takeover = None;
    state.local_playback_state_assertion_pending = false;
}

/// Decide whether an inbound remote queue may mutate the local core queue.
/// A matching, action-confirmed local echo advances the fence but is not
/// materialized: the live core queue/cursor already owns playback. Mismatching
/// bootstrap replies stay suppressed; they never gain authority just because
/// an arbitrary timer elapsed.
pub fn should_materialize_remote_queue(
    state: &mut QconnectRemoteSyncState,
    queue: &QConnectQueueState,
) -> bool {
    // The takeover echo is evaluated BEFORE the conflict gate: accepting it
    // materializes nothing, it only retires the fence that local authority
    // armed. Leaving it behind an unresolved conflict flag dropped the echo of
    // our own publish, and with no further cloud queue state the fence stayed
    // armed until an unrelated local action republished the queue (every
    // renderer command discarded meanwhile).
    let conflict_pending = state.local_playback_conflict_pending;
    let Some(pending) = state.pending_local_queue_takeover.as_mut() else {
        return !conflict_pending;
    };
    let incoming_ids: Vec<u64> = queue.queue_items.iter().map(|item| item.track_id).collect();
    let accepted = incoming_ids == pending.expected_track_ids && pending.action_confirmed;
    if !accepted {
        log::debug!(
            "[QConnect] takeover echo rejected: action_confirmed={} ids_match={} conflict={} expected={:?} incoming={:?}",
            pending.action_confirmed,
            incoming_ids == pending.expected_track_ids,
            conflict_pending,
            pending.expected_track_ids,
            incoming_ids,
        );
        if pending.action_confirmed {
            pending.retry_needed = true;
        }
        return false;
    }
    state.pending_local_queue_takeover = None;
    state.last_applied_queue_state = Some(queue.clone());
    state.local_playback_state_assertion_pending = true;
    // The core already owns this exact queue and cursor. This is an
    // acknowledgement, not an instruction to materialize/seek it again.
    false
}

/// True only when the session's active renderer is a peer and that exact peer
/// has reported the QConnect PLAYING wire state. A merely active but
/// paused/stopped/unknown peer must not stop real local playback.
pub fn active_peer_renderer_is_playing(state: &QconnectRemoteSyncState) -> bool {
    let Some(active_renderer_id) = state.session.active_renderer_id else {
        return false;
    };
    if state.session.local_renderer_id == Some(active_renderer_id) {
        return false;
    }
    state
        .session_renderer_states
        .get(&active_renderer_id)
        .and_then(|renderer| renderer.playing_state)
        == Some(crate::renderer::PLAYING_STATE_PLAYING)
}

/// Local audio yields only after remote authority is fully settled. During a
/// local-queue takeover the old active peer can keep reporting PLAYING until
/// the cloud applies SET_ACTIVE_RENDERER; treating that stale report as current
/// authority would stop the very playback the user chose to preserve.
pub fn local_playback_should_yield_to_active_peer(state: &QconnectRemoteSyncState) -> bool {
    !remote_renderer_commands_are_fenced(state) && active_peer_renderer_is_playing(state)
}

/// Set each cached renderer's `active` flag to match the session's current
/// active renderer id. Pure mutation over the relocated accumulator; relocated
/// from the Tauri adapter (slice 2+4) so the shared session-apply logic in
/// `app.rs` and the Tauri adapter both call one definition.
pub fn sync_session_renderer_active_flags(state: &mut QconnectRemoteSyncState) {
    for (renderer_id, renderer_state) in &mut state.session_renderer_states {
        renderer_state.active = state
            .session
            .active_renderer_id
            .map(|active_renderer_id| active_renderer_id == *renderer_id);
    }
}

/// Get-or-insert the cached per-renderer state for `renderer_id`, seeding its
/// `active` flag from the session's current active renderer. Pure mutation;
/// relocated from the Tauri adapter (slice 2+4). Byte-identical behavior.
pub fn ensure_session_renderer_state(
    state: &mut QconnectRemoteSyncState,
    renderer_id: i32,
) -> &mut QconnectSessionRendererState {
    let active = state
        .session
        .active_renderer_id
        .map(|active_renderer_id| active_renderer_id == renderer_id);
    state
        .session_renderer_states
        .entry(renderer_id)
        .or_insert_with(|| QconnectSessionRendererState {
            active,
            ..Default::default()
        })
}

/// Cache the queue_item/track ids + playing_state derived from a renderer
/// snapshot into the accumulator, so subsequent outbound reports and visible
/// projections reuse them. Pure mutation; relocated from the Tauri adapter
/// (slice 6, Slint port) so both the Tauri and Slint event sinks share one
/// definition. Byte-identical behavior.
pub fn cache_renderer_snapshot(
    state: &mut QconnectRemoteSyncState,
    renderer_snapshot: &QConnectRendererState,
) {
    state.last_renderer_queue_item_id = renderer_snapshot
        .current_track
        .as_ref()
        .map(|item| item.queue_item_id);
    state.last_renderer_next_queue_item_id = renderer_snapshot
        .next_track
        .as_ref()
        .map(|item| item.queue_item_id);
    state.last_renderer_track_id = renderer_snapshot
        .current_track
        .as_ref()
        .map(|item| item.track_id);
    state.last_renderer_next_track_id = renderer_snapshot
        .next_track
        .as_ref()
        .map(|item| item.track_id);
    state.last_renderer_playing_state = renderer_snapshot.playing_state;
}

#[cfg(test)]
mod tests {
    use qconnect_core::{QConnectQueueState, QueueItem};

    use super::*;

    fn queue(track_ids: &[u64]) -> QConnectQueueState {
        QConnectQueueState {
            queue_items: track_ids
                .iter()
                .enumerate()
                .map(|(index, track_id)| QueueItem {
                    track_context_uuid: String::new(),
                    track_id: *track_id,
                    queue_item_id: index as u64 + 1,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn local_takeover_suppresses_stale_queue_but_accepts_its_echo() {
        let mut state = QconnectRemoteSyncState::default();
        arm_local_queue_takeover(&mut state, vec![10, 20], "load-1".to_string());

        assert!(!should_materialize_remote_queue(
            &mut state,
            &queue(&[90, 91])
        ));
        assert!(!should_materialize_remote_queue(
            &mut state,
            &queue(&[10, 20])
        ));
        assert!(confirm_local_queue_takeover_action(&mut state, "load-1"));
        assert!(!should_materialize_remote_queue(
            &mut state,
            &queue(&[10, 20])
        ));
        assert!(state.pending_local_queue_takeover.is_none());
        assert!(state.local_playback_state_assertion_pending);
        assert!(remote_renderer_commands_are_fenced(&state));
        confirm_local_playback_state_asserted(&mut state);
        assert!(!remote_renderer_commands_are_fenced(&state));
    }

    #[test]
    fn stale_queue_arms_retry_without_expiring_local_authority() {
        let mut state = QconnectRemoteSyncState::default();
        arm_local_queue_takeover(&mut state, vec![10, 20], "load-1".to_string());

        assert!(!should_materialize_remote_queue(
            &mut state,
            &queue(&[90, 91])
        ));
        assert!(!local_queue_takeover_needs_retry(&state));
        assert!(reject_local_queue_takeover_action(&mut state, "load-1"));
        assert!(local_queue_takeover_needs_retry(&state));
        assert!(!should_materialize_remote_queue(
            &mut state,
            &queue(&[90, 91])
        ));
    }

    #[test]
    fn same_tracks_do_not_complete_takeover_without_matching_action_ack() {
        let mut state = QconnectRemoteSyncState::default();
        arm_local_queue_takeover(&mut state, vec![10, 20], "load-new".to_string());

        assert!(!confirm_local_queue_takeover_action(&mut state, "load-old"));
        assert!(!should_materialize_remote_queue(
            &mut state,
            &queue(&[10, 20])
        ));
        assert!(state.pending_local_queue_takeover.is_some());
    }

    /// Reconnecting while local audio plays arms the conflict flag. The echo of
    /// the queue local authority just published must still retire the fence:
    /// otherwise every renderer command is discarded until an unrelated local
    /// action republishes the queue.
    #[test]
    fn takeover_echo_retires_the_fence_even_with_the_conflict_flag_armed() {
        let mut state = QconnectRemoteSyncState::default();
        set_local_playback_conflict_pending(&mut state, true);
        arm_local_queue_takeover(&mut state, vec![10, 20], "load-1".to_string());
        assert!(confirm_local_queue_takeover_action(&mut state, "load-1"));

        assert!(!should_materialize_remote_queue(
            &mut state,
            &queue(&[10, 20])
        ));
        assert!(state.pending_local_queue_takeover.is_none());
        assert!(state.local_playback_state_assertion_pending);
        confirm_local_playback_state_asserted(&mut state);
        set_local_playback_conflict_pending(&mut state, false);
        assert!(!remote_renderer_commands_are_fenced(&state));
    }

    #[test]
    fn unresolved_playback_conflict_blocks_every_remote_queue() {
        let mut state = QconnectRemoteSyncState::default();
        set_local_playback_conflict_pending(&mut state, true);
        assert!(!should_materialize_remote_queue(
            &mut state,
            &queue(&[90, 91])
        ));
    }

    #[test]
    fn only_a_playing_active_peer_requires_local_playback_to_yield() {
        let mut state = QconnectRemoteSyncState::default();
        state.session.local_renderer_id = Some(1);
        state.session.active_renderer_id = Some(2);
        state.session_renderer_states.insert(
            2,
            QconnectSessionRendererState {
                playing_state: Some(crate::renderer::PLAYING_STATE_PAUSED),
                ..Default::default()
            },
        );
        assert!(!active_peer_renderer_is_playing(&state));

        state
            .session_renderer_states
            .get_mut(&2)
            .expect("peer state")
            .playing_state = Some(crate::renderer::PLAYING_STATE_PLAYING);
        assert!(active_peer_renderer_is_playing(&state));
        assert!(local_playback_should_yield_to_active_peer(&state));

        // Option 3 has chosen local authority, but the old peer can continue
        // reporting PLAYING until SET_ACTIVE_RENDERER reaches every client.
        // That stale window must never stop the live local player.
        arm_local_queue_takeover(&mut state, vec![10, 20], "load-local".to_string());
        assert!(!local_playback_should_yield_to_active_peer(&state));
        assert!(confirm_local_queue_takeover_action(
            &mut state,
            "load-local"
        ));
        assert!(!should_materialize_remote_queue(
            &mut state,
            &queue(&[10, 20])
        ));
        assert!(state.local_playback_state_assertion_pending);
        assert!(!local_playback_should_yield_to_active_peer(&state));

        state.session.active_renderer_id = Some(1);
        assert!(!active_peer_renderer_is_playing(&state));
    }
}
