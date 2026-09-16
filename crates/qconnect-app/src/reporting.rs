//! Shared renderer playback-report projection.
//!
//! Frontend adapters provide a player snapshot and queue-item resolution. This
//! module alone owns the QConnect buffer mapping and JSON payload shape.

use qbz_player::player::{PlaybackBufferState, PlaybackEvent};
use qconnect_core::QueueVersion;
use qconnect_protocol::{RendererBufferState, RendererReport, RendererReportType};

use crate::renderer::{PLAYING_STATE_PAUSED, PLAYING_STATE_PLAYING};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RendererPlaybackSnapshot {
    pub playing_state: i32,
    pub buffer_state: PlaybackBufferState,
    pub position_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub current_queue_item_id: Option<u64>,
    pub next_queue_item_id: Option<u64>,
}

pub const fn renderer_buffer_state(state: PlaybackBufferState) -> RendererBufferState {
    match state {
        PlaybackBufferState::Idle | PlaybackBufferState::Ready => RendererBufferState::Ok,
        PlaybackBufferState::InitialBuffering => RendererBufferState::Buffering,
        PlaybackBufferState::Underrun => RendererBufferState::Underrun,
        PlaybackBufferState::Error => RendererBufferState::Error,
    }
}

/// Project player state onto the official renderer intent semantics.
///
/// During initial buffering the audio device has not started yet, so
/// `PlaybackEvent::is_playing` is false. Official clients nevertheless retain
/// the requested PLAYING intent alongside BUFFERING. An underrun likewise
/// remains PLAYING while audio is temporarily starved.
pub const fn renderer_playing_state(is_playing: bool, buffer_state: PlaybackBufferState) -> i32 {
    if matches!(
        buffer_state,
        PlaybackBufferState::InitialBuffering | PlaybackBufferState::Underrun
    ) || is_playing
    {
        PLAYING_STATE_PLAYING
    } else {
        PLAYING_STATE_PAUSED
    }
}

/// Pick the identity atomically paired with buffer state while a play is
/// loading. Once idle, the audible/current track remains authoritative.
pub const fn qconnect_report_track_id(event: &PlaybackEvent) -> u64 {
    if !matches!(event.buffer_state, PlaybackBufferState::Idle) && event.buffer_track_id != 0 {
        event.buffer_track_id
    } else {
        event.track_id
    }
}

/// Resolve a report without mutating the queue. An exact retained item identity
/// wins; a track id alone is usable only when it names one occurrence.
pub fn resolve_report_queue_items(
    queue: &crate::QConnectQueueState,
    track_id: u64,
    current_hint: Option<u64>,
    next_hint: Option<u64>,
) -> (Option<u64>, Option<u64>, Option<u64>) {
    use crate::queue_resolution::*;
    let cursors = ordered_queue_cursors(queue);
    let matching =
        |index: usize| queue_item_track_id_for_cursor(queue, cursors[index]) == Some(track_id);
    let hinted = [current_hint, next_hint].into_iter().find_map(|hint| {
        find_cursor_index_by_queue_item_id(&cursors, queue, hint).filter(|index| matching(*index))
    });
    let index = hinted.or_else(|| {
        let mut hits = (0..cursors.len()).filter(|index| matching(*index));
        let first = hits.next()?;
        hits.next().is_none().then_some(first)
    });
    let Some(index) = index else {
        return (None, None, None);
    };
    let next = cursors.get(index + 1).copied();
    (
        normalized_queue_item_id_for_cursor(queue, cursors[index]),
        next.and_then(|c| normalized_queue_item_id_for_cursor(queue, c)),
        next.and_then(|c| queue_item_track_id_for_cursor(queue, c)),
    )
}

/// Queue ids for the report that ACKNOWLEDGES a renderer command.
///
/// They were omitted outright (40a2fa618: the server rejected ids it could not
/// match after a QBZ-initiated queue load), but an omitted scalar is 0 on the
/// wire and 0 is the FIRST queue item, so every command-driven track change
/// bounced the controller to the head of the queue until the next periodic
/// report corrected it. Resolving against the synchronized queue — the same
/// resolution the periodic report uses — keeps that protection: an occurrence
/// that does not resolve is still reported as absent.
pub fn acknowledged_queue_item_ids(
    queue: &crate::QConnectQueueState,
    renderer: &crate::QConnectRendererState,
) -> (Option<u64>, Option<u64>) {
    let Some(track) = renderer.current_track.as_ref() else {
        return (None, None);
    };
    let (current, next, _) = resolve_report_queue_items(
        queue,
        track.track_id,
        Some(track.queue_item_id),
        renderer.next_track.as_ref().map(|item| item.queue_item_id),
    );
    (current, next)
}

/// One real-state projection for regular reports, activation and reconnect.
pub fn playback_snapshot_from_event(
    event: &PlaybackEvent,
    queue: &crate::QConnectQueueState,
    current_hint: Option<u64>,
    next_hint: Option<u64>,
) -> RendererPlaybackSnapshot {
    let id = qconnect_report_track_id(event);
    let (current, next, _) = resolve_report_queue_items(queue, id, current_hint, next_hint);
    let same_audio = id != 0 && id == event.track_id;
    RendererPlaybackSnapshot {
        playing_state: if id == 0
            || (!event.is_playing && event.buffer_state == PlaybackBufferState::Idle)
        {
            crate::renderer::PLAYING_STATE_STOPPED
        } else {
            renderer_playing_state(event.is_playing, event.buffer_state)
        },
        buffer_state: event.buffer_state,
        // During a new load, position/duration from the outgoing audio are stale.
        position_ms: same_audio
            .then(|| crate::qconnect_millis_from_secs(event.position).min(i64::MAX as u64) as i64),
        duration_ms: same_audio
            .then(|| crate::qconnect_millis_from_secs(event.duration).min(i64::MAX as u64) as i64),
        current_queue_item_id: current,
        next_queue_item_id: next,
    }
}

/// Keep the live position for a same-track queue echo already rejected by
/// the seek handler. Position-only seeks and track changes retain their intent.
pub fn preserve_running_position(
    command: &mut crate::RendererCommand,
    event: &PlaybackEvent,
    previous_queue_item_id: Option<u64>,
) -> bool {
    let crate::RendererCommand::SetState {
        playing_state,
        current_track,
        current_position_ms,
        ..
    } = command
    else {
        return false;
    };
    let matching_intent = (*playing_state == Some(PLAYING_STATE_PLAYING) && event.is_playing)
        || (*playing_state == Some(PLAYING_STATE_PAUSED)
            && !event.is_playing
            && event.buffer_state == PlaybackBufferState::Ready);
    if matching_intent
        && event.position > 2
        && current_track.as_ref().is_some_and(|track| {
            track.track_id == event.track_id
                && previous_queue_item_id.is_none_or(|id| id == track.queue_item_id)
        })
        && current_position_ms.is_some_and(|position| position <= 1000)
    {
        *current_position_ms = Some(crate::qconnect_millis_from_secs(event.position));
        return true;
    }
    false
}

pub fn build_renderer_playback_report(
    action_uuid: impl Into<String>,
    queue_version: QueueVersion,
    snapshot: RendererPlaybackSnapshot,
) -> RendererReport {
    RendererReport::new(
        RendererReportType::RndrSrvrStateUpdated,
        action_uuid,
        queue_version,
        serde_json::json!({
            "playing_state": snapshot.playing_state,
            "buffer_state": renderer_buffer_state(snapshot.buffer_state).as_i32(),
            "current_position": snapshot.position_ms,
            "duration": snapshot.duration_ms,
            "current_queue_item_id": snapshot.current_queue_item_id,
            "next_queue_item_id": snapshot.next_queue_item_id,
            "queue_version": {
                "major": queue_version.major,
                "minor": queue_version.minor
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue() -> crate::QConnectQueueState {
        crate::QConnectQueueState {
            queue_items: [10, 20, 10, 30]
                .into_iter()
                .enumerate()
                .map(|(i, id)| qconnect_core::QueueItem {
                    track_context_uuid: String::new(),
                    track_id: id,
                    queue_item_id: 100 + i as u64,
                })
                .collect(),
            ..Default::default()
        }
    }
    #[test]
    fn reporting_preserves_duplicate_occurrence_and_never_guesses_ambiguity() {
        let q = queue();
        assert_eq!(
            resolve_report_queue_items(&q, 10, Some(102), None),
            (Some(102), Some(103), Some(30))
        );
        assert_eq!(
            resolve_report_queue_items(&q, 10, Some(101), Some(102)),
            (Some(102), Some(103), Some(30))
        );
        assert_eq!(
            resolve_report_queue_items(&q, 10, None, None),
            (None, None, None)
        );
        assert_eq!(
            resolve_report_queue_items(&q, 20, None, None),
            (Some(101), Some(102), Some(10))
        );
    }
    /// The acknowledgement report must name the occurrence the command asked
    /// for. Omitting it reads as queue item 0 — the head of the queue — and the
    /// controller visibly jumps there on every track change.
    #[test]
    fn acknowledgement_names_the_requested_occurrence_or_stays_absent() {
        let q = queue();
        let item = |track_id: u64, queue_item_id: u64| qconnect_core::QueueItem {
            track_context_uuid: String::new(),
            track_id,
            queue_item_id,
        };

        let renderer = crate::QConnectRendererState {
            current_track: Some(item(10, 102)),
            next_track: Some(item(30, 103)),
            ..Default::default()
        };
        assert_eq!(
            acknowledged_queue_item_ids(&q, &renderer),
            (Some(102), Some(103))
        );

        // An occurrence absent from the synchronized queue is never invented.
        let unknown = crate::QConnectRendererState {
            current_track: Some(item(99, 999)),
            ..Default::default()
        };
        assert_eq!(acknowledged_queue_item_ids(&q, &unknown), (None, None));

        // A state-only command carries no track: nothing to name.
        assert_eq!(
            acknowledged_queue_item_ids(&q, &crate::QConnectRendererState::default()),
            (None, None)
        );
    }

    #[test]
    fn reporting_uses_shuffle_successor() {
        let mut q = queue();
        q.shuffle_mode = true;
        q.shuffle_order = Some(vec![0, 2, 1, 3]);
        assert_eq!(
            resolve_report_queue_items(&q, 10, Some(102), None),
            (Some(102), Some(101), Some(20))
        );
    }
    #[test]
    fn reconnect_preserves_paused_position_and_buffer_lifecycle() {
        let q = queue();
        let mut event = PlaybackEvent {
            track_id: 20,
            position: 45,
            duration: 180,
            buffer_state: PlaybackBufferState::Ready,
            buffer_track_id: 20,
            ..Default::default()
        };
        let paused = playback_snapshot_from_event(&event, &q, Some(101), None);
        assert_eq!(paused.playing_state, PLAYING_STATE_PAUSED);
        assert_eq!(paused.position_ms, Some(45_000));
        for state in [
            PlaybackBufferState::InitialBuffering,
            PlaybackBufferState::Underrun,
        ] {
            event.buffer_state = state;
            let snapshot = playback_snapshot_from_event(&event, &q, Some(101), None);
            assert_eq!(snapshot.playing_state, PLAYING_STATE_PLAYING);
            assert_eq!(snapshot.buffer_state, state);
        }
        event.buffer_state = PlaybackBufferState::Idle;
        assert_eq!(
            playback_snapshot_from_event(&event, &q, Some(101), None).playing_state,
            crate::renderer::PLAYING_STATE_STOPPED
        );
    }
    #[test]
    fn new_load_does_not_borrow_outgoing_position_or_duration() {
        let event = PlaybackEvent {
            track_id: 20,
            position: 160,
            duration: 180,
            buffer_track_id: 30,
            buffer_state: PlaybackBufferState::InitialBuffering,
            ..Default::default()
        };
        let snapshot = playback_snapshot_from_event(&event, &queue(), Some(103), None);
        assert_eq!(snapshot.current_queue_item_id, Some(103));
        assert_eq!(snapshot.position_ms, None);
        assert_eq!(snapshot.duration_ms, None);
        let report = build_renderer_playback_report("activate", Default::default(), snapshot);
        assert_eq!(report.payload["buffer_state"], 1);
        assert!(report.payload.get("is_active").is_none());
    }
    #[test]
    fn queue_echo_preserves_progress_without_swallowing_seek_or_selection() {
        let event = PlaybackEvent {
            track_id: 20,
            is_playing: true,
            position: 67,
            buffer_state: PlaybackBufferState::Ready,
            ..Default::default()
        };
        let mut command = crate::RendererCommand::SetState {
            playing_state: Some(PLAYING_STATE_PLAYING),
            current_position_ms: Some(0),
            current_track: Some(qconnect_core::QueueItem {
                track_id: 20,
                queue_item_id: 101,
                track_context_uuid: String::new(),
            }),
            next_track: None,
        };
        assert!(preserve_running_position(&mut command, &event, Some(101)));
        let crate::RendererCommand::SetState {
            current_position_ms,
            ..
        } = &command
        else {
            panic!()
        };
        assert_eq!(*current_position_ms, Some(67_000));
        let mut different_occurrence = crate::RendererCommand::SetState {
            playing_state: Some(PLAYING_STATE_PLAYING),
            current_position_ms: Some(0),
            current_track: Some(qconnect_core::QueueItem {
                track_id: 20,
                queue_item_id: 102,
                track_context_uuid: String::new(),
            }),
            next_track: None,
        };
        assert!(!preserve_running_position(
            &mut different_occurrence,
            &event,
            Some(101)
        ));

        for (track, position, playing) in [
            (None, 0, None),
            (Some(30), 0, Some(PLAYING_STATE_PLAYING)),
            (Some(20), 30_000, Some(PLAYING_STATE_PLAYING)),
            (Some(20), 0, Some(PLAYING_STATE_PAUSED)),
        ] {
            let mut intent = crate::RendererCommand::SetState {
                playing_state: playing,
                current_position_ms: Some(position),
                current_track: track.map(|track_id| qconnect_core::QueueItem {
                    track_id,
                    queue_item_id: 101,
                    track_context_uuid: String::new(),
                }),
                next_track: None,
            };
            assert!(!preserve_running_position(&mut intent, &event, Some(101)));
            let crate::RendererCommand::SetState {
                current_position_ms,
                ..
            } = intent
            else {
                panic!()
            };
            assert_eq!(current_position_ms, Some(position));
        }
    }

    #[test]
    fn player_buffer_states_map_to_official_wire_values() {
        assert_eq!(renderer_buffer_state(PlaybackBufferState::Idle).as_i32(), 2);
        assert_eq!(
            renderer_buffer_state(PlaybackBufferState::InitialBuffering).as_i32(),
            1
        );
        assert_eq!(
            renderer_buffer_state(PlaybackBufferState::Ready).as_i32(),
            2
        );
        assert_eq!(
            renderer_buffer_state(PlaybackBufferState::Underrun).as_i32(),
            4
        );
        assert_eq!(
            renderer_buffer_state(PlaybackBufferState::Error).as_i32(),
            3
        );
    }

    #[test]
    fn buffering_and_underrun_preserve_playing_intent() {
        assert_eq!(
            renderer_playing_state(false, PlaybackBufferState::InitialBuffering),
            PLAYING_STATE_PLAYING
        );
        assert_eq!(
            renderer_playing_state(false, PlaybackBufferState::Underrun),
            PLAYING_STATE_PLAYING
        );
        assert_eq!(
            renderer_playing_state(false, PlaybackBufferState::Ready),
            PLAYING_STATE_PAUSED
        );
        assert_eq!(
            renderer_playing_state(true, PlaybackBufferState::Ready),
            PLAYING_STATE_PLAYING
        );
    }

    #[test]
    fn playback_report_builder_owns_the_wire_shape() {
        let version = QueueVersion { major: 7, minor: 3 };
        let report = build_renderer_playback_report(
            "action",
            version,
            RendererPlaybackSnapshot {
                playing_state: 2,
                buffer_state: PlaybackBufferState::Underrun,
                position_ms: Some(118_000),
                duration_ms: Some(317_000),
                current_queue_item_id: Some(4),
                next_queue_item_id: Some(5),
            },
        );

        assert_eq!(report.report_type, RendererReportType::RndrSrvrStateUpdated);
        assert_eq!(report.payload["buffer_state"], 4);
        assert_eq!(report.payload["current_position"], 118_000);
        assert_eq!(report.payload["duration"], 317_000);
        assert_eq!(report.payload["queue_version"]["major"], 7);
        assert_eq!(report.payload["queue_version"]["minor"], 3);
    }
}
