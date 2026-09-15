//! Shared renderer orchestration and queue admission for the Qt and daemon hosts.
//! Transport intent, queue occurrences and engine acceptance remain distinct.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use qbz_models::{QueueTrack, RepeatMode, Track};
use qbz_player::PlaybackState;
use tokio::sync::Mutex;

use crate::queue_resolution::{
    dedupe_track_ids, resolve_core_shuffle_order, resolve_remote_start_index,
};
use crate::renderer_engine::QconnectRendererEngine;
use crate::session::quality_from_max_audio_quality;
use crate::{QConnectQueueState, QConnectRendererState, QconnectRemoteSyncState, RendererCommand};

/// QConnect protocol `playing_state` wire values. Single source of truth for the
/// renderer orchestration; the Tauri adapter re-exports these from here.
pub const PLAYING_STATE_UNKNOWN: i32 = 0;
pub const PLAYING_STATE_STOPPED: i32 = 1;
pub const PLAYING_STATE_PLAYING: i32 = 2;
pub const PLAYING_STATE_PAUSED: i32 = 3;

/// Dedup window: an echoed SetState for a track whose load was registered within
/// this window does not re-trigger the load. The audio thread updates
/// `playback_state.track_id` only after the engine appends the source, so a bare
/// `track_id` comparison would re-fire during that buffer/decode gap.
const LOAD_ATTEMPT_DEDUP_WINDOW: Duration = Duration::from_secs(5);

/// Canonical source stamped on tracks materialized from a QConnect cloud queue.
///
/// The wire supplies Qobuz catalog ids. Keeping the old transport-only
/// `qobuz_connect_remote` tag in the core queue leaked QConnect lifecycle state
/// into downstream renderers: after a manual disconnect, Cast no longer chose
/// its Qobuz progressive-stream path. Materialized rows therefore use the same
/// portable provenance as every other Qobuz catalog row.
pub const QCONNECT_REMOTE_QUEUE_SOURCE: &str = "qobuz";

/// Backward-compatible spelling found in persisted queues and in queues
/// materialized by an older runtime before an in-process upgrade/reconnect.
pub const LEGACY_QCONNECT_REMOTE_QUEUE_SOURCE: &str = "qobuz_connect_remote";

/// Whether a queue row can be named by Qobuz Connect without looking the id up.
///
/// Source provenance is the authority: local-library and media-server ids can
/// overlap the numeric Qobuz id space, while offline Qobuz downloads retain the
/// real catalog id and remain resolvable. A missing source is accepted only by
/// the [`QueueTrack`] wrapper below, where the legacy `is_local` discriminator
/// is still available.
pub fn qconnect_source_is_resolvable(track_id: u64, source: Option<&str>) -> bool {
    if track_id == 0 {
        return false;
    }
    match source.map(str::trim).filter(|source| !source.is_empty()) {
        Some(source) => matches!(
            source.to_ascii_lowercase().as_str(),
            QCONNECT_REMOTE_QUEUE_SOURCE
                | "qobuz_download"
                | "qobuz_purchase"
                | "offline"
                | LEGACY_QCONNECT_REMOTE_QUEUE_SOURCE
        ),
        // Callers with only the wire-shaped `(id, source)` pair have no local
        // bit. Every current local producer stamps an explicit source, so the
        // source-less shape is the legacy catalog case.
        None => true,
    }
}

/// Queue-row form of [`qconnect_source_is_resolvable`]. This is the canonical
/// admission predicate for desktop queue filtering and daemon publication.
pub fn qconnect_queue_track_is_resolvable(track: &QueueTrack) -> bool {
    match track
        .source
        .as_deref()
        .map(str::trim)
        .filter(|source| !source.is_empty())
    {
        Some(source) => qconnect_source_is_resolvable(track.id, Some(source)),
        None => track.id > 0 && !track.is_local,
    }
}

pub fn qconnect_repeat_mode_from_loop_mode(loop_mode: i32) -> Option<RepeatMode> {
    // QConnect protocol loop mode values: 1 = off, 2 = repeat one, 3 = repeat all.
    match loop_mode {
        0 | 1 => Some(RepeatMode::Off),
        2 => Some(RepeatMode::One),
        3 => Some(RepeatMode::All),
        _ => None,
    }
}

pub fn normalize_volume_to_fraction(volume: i32) -> f32 {
    volume.clamp(0, 100) as f32 / 100.0
}

pub fn model_track_to_core_queue_track(track: &Track) -> QueueTrack {
    let artwork_url = track
        .album
        .as_ref()
        .and_then(|album| album.image.best().cloned());
    let artist = track
        .performer
        .as_ref()
        .map(|performer| performer.name.clone())
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let album = track
        .album
        .as_ref()
        .map(|album| album.title.clone())
        .unwrap_or_else(|| "Unknown Album".to_string());
    let album_id = track.album.as_ref().and_then(|album| {
        let trimmed = album.id.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });
    let artist_id = track.performer.as_ref().map(|performer| performer.id);

    QueueTrack {
        id: track.id,
        title: track.title.clone(),
        version: track.version.clone(),
        artist,
        album,
        album_version: None,
        duration_secs: track.duration as u64,
        artwork_url,
        hires: track.hires,
        bit_depth: track.maximum_bit_depth,
        sample_rate: track.maximum_sampling_rate,
        is_local: false,
        album_id: album_id.clone(),
        artist_id,
        // A queue pushed by a QConnect peer carries real Qobuz tracks, so the
        // flag is worth keeping — but resolved through `is_streamable()`, since
        // a peer's payload is one more endpoint we have never captured and a
        // terse one must not arrive here marked dead. Its source is canonical
        // Qobuz provenance, not a transport-lifecycle tag: this queue remains
        // playable by local and Cast renderers after QConnect disconnects.
        streamable: track.is_streamable(),
        source: Some(QCONNECT_REMOTE_QUEUE_SOURCE.to_string()),
        parental_warning: track.parental_warning,
        source_item_id_hint: album_id,
        context_kind: None,
        context_id: None,
        isrc: None,
        recording_mbid: None,
    }
}

/// Keep intrinsic metadata that was already present in the local queue when a
/// QConnect cloud echo materializes the same catalog track again.
///
/// The queue wire carries ids only. Hydrating those ids through the batch
/// track endpoint gives us enough to play, but that endpoint can omit the
/// track `version` and cannot express the album release `version` at all
/// (`Track::album` is only an `AlbumSummary`). Replacing an album-built queue
/// with that terse projection therefore stripped edition suffixes from every
/// downstream consumer: queue rows, Now Playing, MPRIS, lyrics and scrobbling.
///
/// Only missing intrinsic fields are filled, and only from a non-local track
/// with the same catalog id. The remote source tag, availability answer and
/// context remain authoritative; in particular, an unrelated local-library id
/// collision can never leak file metadata into a Qobuz queue.
fn preserve_existing_catalog_metadata(remote: &mut QueueTrack, existing: &QueueTrack) {
    if remote.id != existing.id || existing.is_local {
        return;
    }

    let missing = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    };
    if missing(&remote.version) && !missing(&existing.version) {
        remote.version = existing.version.clone();
    }
    if missing(&remote.album_version) && !missing(&existing.album_version) {
        remote.album_version = existing.album_version.clone();
    }
    if remote.title.trim().is_empty() && !existing.title.trim().is_empty() {
        remote.title = existing.title.clone();
    }
    if (remote.artist.trim().is_empty() || remote.artist == "Unknown Artist")
        && !existing.artist.trim().is_empty()
    {
        remote.artist = existing.artist.clone();
    }
    if (remote.album.trim().is_empty() || remote.album == "Unknown Album")
        && !existing.album.trim().is_empty()
    {
        remote.album = existing.album.clone();
    }
    if missing(&remote.artwork_url) && !missing(&existing.artwork_url) {
        remote.artwork_url = existing.artwork_url.clone();
    }
    if missing(&remote.album_id) && !missing(&existing.album_id) {
        remote.album_id = existing.album_id.clone();
    }
    if remote.artist_id.is_none() {
        remote.artist_id = existing.artist_id;
    }
    if remote.duration_secs == 0 {
        remote.duration_secs = existing.duration_secs;
    }
    if remote.bit_depth.is_none() {
        remote.bit_depth = existing.bit_depth;
    }
    if remote.sample_rate.is_none() {
        remote.sample_rate = existing.sample_rate;
    }
    remote.hires |= existing.hires;
}

// Shared orchestration. Host adapters provide execution and authority checks.

pub fn queue_state_needs_materialization(
    previous: Option<&QConnectQueueState>,
    next: &QConnectQueueState,
) -> bool {
    let Some(previous) = previous else {
        return true;
    };

    previous.version != next.version
        || previous.queue_items != next.queue_items
        || previous.shuffle_mode != next.shuffle_mode
        || previous.shuffle_order != next.shuffle_order
        || previous.autoplay_mode != next.autoplay_mode
        || previous.autoplay_loading != next.autoplay_loading
        || previous.autoplay_items != next.autoplay_items
}

pub fn should_reload_remote_track(playback_state: &PlaybackState, track_id: u64) -> bool {
    // Only reload when the track ID actually changed. The previous
    // !has_loaded_audio gate fired during the buffering window of an
    // initial load (qbz already started fetching but the audio engine
    // hasn't reported the track as loaded yet) — when the cloud echo
    // SetState arrived for the same track, this caused a redundant
    // load that interrupted the in-progress one. That was the residual
    // first-track hiccup.
    playback_state.track_id != track_id
}

/// Returns true if a load attempt for `track_id` was registered within the
/// dedup window (see `LOAD_ATTEMPT_DEDUP_WINDOW`).
fn is_recent_load_attempt(state: &QconnectRemoteSyncState, track_id: u64) -> bool {
    match state.last_load_attempt {
        Some((tid, ts)) => tid == track_id && ts.elapsed() < LOAD_ATTEMPT_DEDUP_WINDOW,
        None => false,
    }
}

/// Load a remote track into the engine, deduped against echoed SetState frames.
/// Records the attempt BEFORE dispatching the load (the audio thread updates
/// `playback_state.track_id` only after the engine appends the source, so the
/// recording must precede the load to close the echo window).
///
/// `start_position_secs` is the position to resume the stream at. For a normal
/// peer track-change the cloud sends position ~0, so this is 0 (a fresh track).
/// On a TAKEBACK whose first load lands here (SetActive arrived before the cloud
/// knew our current_track, so the force-stream couldn't fire), the SetState
/// carries the peer's real position and we resume there instead of streaming
/// from 0 and then trying to seek forward — a forward seek past the buffered
/// watermark is silently ignored by the audio thread, so streaming from 0 left
/// the first takeback playing from the start (bad for audiobooks). The protected
/// bit-perfect seams + the HTTP feeder live behind `start_track_stream`.
pub async fn ensure_remote_track_loaded(
    engine: &impl QconnectRendererEngine,
    sync_state: &Arc<Mutex<QconnectRemoteSyncState>>,
    track_id: u64, max_audio_quality: Option<i32>, start_position_secs: u64,
) -> Result<bool, String> {
    prepare_remote_track(engine, sync_state, track_id, max_audio_quality, start_position_secs, true, false).await
}

pub async fn force_remote_track_stream(
    engine: &impl QconnectRendererEngine,
    sync_state: &Arc<Mutex<QconnectRemoteSyncState>>,
    track_id: u64, max_audio_quality: Option<i32>, start_position_secs: u64,
) -> Result<bool, String> {
    ensure_remote_track_loaded(engine, sync_state, track_id, max_audio_quality, start_position_secs).await
}

async fn prepare_remote_track(
    engine: &impl QconnectRendererEngine,
    sync_state: &Arc<Mutex<QconnectRemoteSyncState>>,
    track_id: u64, max_audio_quality: Option<i32>, start_position_secs: u64, playing: bool, force: bool,
) -> Result<bool, String> {
    use qbz_player::player::PlaybackBufferState;
    let buffer = engine.loading_state();
    let failed = matches!(buffer, Some((id, PlaybackBufferState::Error)) if id == track_id);
    if !force && !failed && engine.get_playback_state().track_id == track_id && engine.has_loaded_audio() {
        return Ok(false);
    }
    let pending = matches!(buffer, Some((id, PlaybackBufferState::InitialBuffering)) if id == track_id);
    if !force && (pending || (buffer.is_none() && is_recent_load_attempt(&*sync_state.lock().await, track_id))) {
        return Ok(false);
    }
    let generation = sync_state.lock().await.renderer_generation;
    let attempt = (track_id, Instant::now());
    sync_state.lock().await.last_load_attempt = Some(attempt);
    let result = async {
        let track = engine.get_track(track_id).await?;
        if sync_state.lock().await.renderer_generation != generation {
            return Err("renderer activation changed during catalog lookup".into());
        }
        engine.start_track_stream_with_state(
            track_id, quality_from_max_audio_quality(max_audio_quality),
            u64::from(track.duration), start_position_secs, playing,
        ).await
    }.await;
    if result.is_err() {
        let mut state = sync_state.lock().await;
        if state.last_load_attempt == Some(attempt) { state.last_load_attempt = None; }
    }
    result.map(|()| true)
}

pub async fn apply_remote_loop_mode(
    engine: &impl QconnectRendererEngine,
    loop_mode: i32,
) -> Result<(), String> {
    let repeat_mode = qconnect_repeat_mode_from_loop_mode(loop_mode)
        .ok_or_else(|| format!("unsupported qconnect loop mode: {loop_mode}"))?;
    engine.set_repeat_mode(repeat_mode).await;
    Ok(())
}

pub async fn apply_renderer_command(
    engine: &impl QconnectRendererEngine,
    sync_state: &Arc<Mutex<QconnectRemoteSyncState>>,
    command: &RendererCommand,
    renderer_state: &QConnectRendererState,
) -> Result<(), String> {
    match command {
        RendererCommand::SetState {
            playing_state, current_position_ms, current_track, ..
        } => {
            let mut loaded = false;
            let mut new_occurrence = false;
            // Retained state can complete a fresh, partial target. Only explicit
            // fields request transport operations on already loaded audio.
            let desired = playing_state.or(renderer_state.playing_state);
            let target = current_track.as_ref().or(renderer_state.current_track.as_ref());
            let partial_target = {
                let mut sync = sync_state.lock().await;
                if let Some(track) = current_track.as_ref().filter(|_| desired.is_none()) {
                    sync.pending_transport_target = Some((sync.renderer_generation, track.clone()));
                }
                sync.pending_transport_target.as_ref().is_some_and(|(generation, pending)|
                    *generation == sync.renderer_generation && target == Some(pending))
            };
            if matches!(desired, Some(PLAYING_STATE_PLAYING | PLAYING_STATE_PAUSED)) {
                if let Some(track) = target.filter(|_| current_track.is_some() || partial_target || !engine.has_loaded_audio()) {
                    // Align only once transport intent is known. If the target
                    // arrived first, this preserves detection of a later move
                    // to another occurrence of the same recording.
                    let moved = align_remote_occurrence(engine, sync_state, track).await?;
                    if moved && engine.get_playback_state().track_id == track.track_id {
                        new_occurrence = true;
                        engine.stop()?;
                        sync_state.lock().await.last_load_attempt = None;
                    }
                    let position = current_position_ms.or(renderer_state.current_position_ms).unwrap_or(0) / 1000;
                    loaded = prepare_remote_track(engine, sync_state, track.track_id,
                        renderer_state.max_audio_quality, position,
                        desired == Some(PLAYING_STATE_PLAYING), new_occurrence).await?;
                    sync_state.lock().await.pending_transport_target = None;
                } else if !engine.has_loaded_audio() && playing_state == &Some(PLAYING_STATE_PLAYING) {
                    // An unloaded retained local cursor is not a remote target.
                    return Ok(());
                }
            }
            if !loaded {
                match playing_state {
                    Some(PLAYING_STATE_PLAYING) if !engine.get_playback_state().is_playing => engine.resume()?,
                    Some(PLAYING_STATE_PAUSED) => engine.pause()?,
                    Some(PLAYING_STATE_STOPPED) => {
                        engine.stop()?;
                        let mut sync = sync_state.lock().await;
                        sync.last_load_attempt = None;
                        sync.pending_transport_target = None;
                    }
                    _ => {}
                }
            }
            // Absence is never a seek. Zero is a valid explicit seek target.
            if !loaded && desired != Some(PLAYING_STATE_STOPPED)
                && (engine.has_loaded_audio() || engine.loading_state().is_some_and(|(_, state)|
                    state == qbz_player::player::PlaybackBufferState::InitialBuffering)) {
                if let Some(position_ms) = current_position_ms {
                    let target = position_ms / 1000;
                    if engine.get_playback_state().position.abs_diff(target) > 2 {
                        engine.seek(target)?;
                    }
                }
            }
        }
        RendererCommand::SetVolume { volume, .. } => {
            if let Some(resolved) = renderer_state.volume.or(*volume) {
                engine.set_volume(normalize_volume_to_fraction(resolved))?;
            }
        }
        RendererCommand::MuteVolume { value } => {
            if *value {
                engine.set_volume(0.0)?;
            } else if let Some(resolved) = renderer_state.volume {
                engine.set_volume(normalize_volume_to_fraction(resolved))?;
            }
        }
        RendererCommand::SetLoopMode { loop_mode } => {
            let resolved_loop_mode = renderer_state.loop_mode.unwrap_or(*loop_mode);
            let repeat_mode = qconnect_repeat_mode_from_loop_mode(resolved_loop_mode)
                .ok_or_else(|| format!("unsupported qconnect loop mode: {resolved_loop_mode}"))?;
            engine.set_repeat_mode(repeat_mode).await;
        }
        RendererCommand::SetActive { active } => {
            // Ownership is not a playback target. A fresh SetState supplies the
            // occurrence, position and transport intent (in either wire order).
            log::debug!("[QConnect] Renderer activation acknowledged: {active}");
        }
        RendererCommand::SetMaxAudioQuality { max_audio_quality } => {
            // Applied on the next load via renderer_state.max_audio_quality
            // (recorded by the core reducer). No immediate re-fetch.
            log::info!("[QConnect] SetMaxAudioQuality => {max_audio_quality}");
        }
        RendererCommand::SetShuffleMode { shuffle_mode } => {
            let enabled = renderer_state.shuffle_mode.unwrap_or(*shuffle_mode);
            // Renderer commands carry only a flag, never the server seed/order.
            // The queue channel is the sole authority allowed to mutate the
            // local queue. Even a "flag-only" core write is unsafe here because
            // it installs identity order and can race after QueueUpdated.
            log::debug!(
                "[QConnect] SetShuffleMode({enabled}) acknowledged; awaiting queue authority"
            );
        }
    }

    Ok(())
}

async fn sync_remote_shuffle_projection(
    engine: &impl QconnectRendererEngine,
    sync_state: &Arc<Mutex<QconnectRemoteSyncState>>,
    queue_state: &QConnectQueueState,
    renderer_state: &QConnectRendererState,
) -> Result<bool, String> {
    if !queue_state.shuffle_mode || queue_state.queue_items.is_empty() {
        return Ok(false);
    }

    let start_index = resolve_remote_start_index(
        queue_state,
        renderer_state
            .current_track
            .as_ref()
            .map(|item| item.queue_item_id),
        renderer_state
            .current_track
            .as_ref()
            .map(|item| item.track_id),
    );
    let Some(start_index) = start_index else {
        return Ok(false);
    };

    let core_shuffle_order = resolve_core_shuffle_order(queue_state);

    // Do not invent an identity shuffle if no WS-authored order is available.
    if core_shuffle_order.is_none() {
        return Ok(false);
    }

    let should_apply = {
        let state = sync_state.lock().await;
        state.last_materialized_start_index != Some(start_index)
            || state.last_materialized_core_shuffle_order != core_shuffle_order
    };
    if !should_apply {
        return Ok(false);
    }

    let (tracks, _) = engine.get_all_queue_tracks().await;
    if tracks.len() != queue_state.queue_items.len() || tracks.is_empty() {
        return Ok(false);
    }

    engine
        .set_queue_with_order(
            tracks,
            Some(start_index),
            queue_state.shuffle_mode,
            core_shuffle_order.clone(),
        )
        .await;

    let mut state = sync_state.lock().await;
    state.last_materialized_start_index = Some(start_index);
    state.last_materialized_core_shuffle_order = core_shuffle_order;
    Ok(true)
}

pub async fn materialize_remote_queue(
    engine: &impl QconnectRendererEngine,
    sync_state: &Arc<Mutex<QconnectRemoteSyncState>>,
    queue_state: &QConnectQueueState,
) -> Result<bool, String> {
    let (
        renderer_queue_item_id,
        renderer_track_id,
        renderer_next_queue_item_id,
        renderer_next_track_id,
        renderer_playing_state,
        should_skip,
    ) = {
        let mut state = sync_state.lock().await;
        if !queue_state_needs_materialization(state.last_applied_queue_state.as_ref(), queue_state)
        {
            (
                state.last_renderer_queue_item_id,
                state.last_renderer_track_id,
                state.last_renderer_next_queue_item_id,
                state.last_renderer_next_track_id,
                state.last_renderer_playing_state,
                true,
            )
        } else {
            state.last_applied_queue_state = Some(queue_state.clone());
            (
                state.last_renderer_queue_item_id,
                state.last_renderer_track_id,
                state.last_renderer_next_queue_item_id,
                state.last_renderer_next_track_id,
                state.last_renderer_playing_state,
                false,
            )
        }
    };

    if should_skip {
        log::debug!(
            "[QConnect] materialize_remote_queue: skipped (identical snapshot {}.{})",
            queue_state.version.major,
            queue_state.version.minor
        );
        return Ok(false);
    }

    log::info!(
        "[QConnect] materialize_remote_queue: version={}.{} items={} renderer_qid={:?} renderer_tid={:?} renderer_next_qid={:?} renderer_next_tid={:?} playing_state={:?}",
        queue_state.version.major,
        queue_state.version.minor,
        queue_state.queue_items.len(),
        renderer_queue_item_id,
        renderer_track_id,
        renderer_next_queue_item_id,
        renderer_next_track_id,
        renderer_playing_state
    );

    if queue_state.queue_items.is_empty() {
        // Preserve legacy behavior: keep current track on qconnect sync clears.
        engine.clear_queue(true).await;
        engine.set_shuffle(false).await;
        let mut state = sync_state.lock().await;
        state.last_materialized_start_index = None;
        state.last_materialized_core_shuffle_order = None;
        return Ok(true);
    }

    // Preserve the richer QueueTrack built by album/playlist entry points
    // before the id-only cloud echo replaces the core queue. Per-id enrichment
    // is safe across reorder/insert operations because every copied field is
    // intrinsic catalog metadata; queue context and the remote source word are
    // deliberately not copied.
    let (existing_tracks, _) = engine.get_all_queue_tracks().await;
    let existing_by_id: HashMap<u64, QueueTrack> = existing_tracks
        .into_iter()
        .filter(|track| !track.is_local)
        .map(|track| (track.id, track))
        .collect();

    let unique_track_ids = dedupe_track_ids(queue_state);
    let fetched_tracks = engine
        .get_tracks_batch(&unique_track_ids)
        .await
        .map_err(|err| format!("fetch tracks batch for remote queue: {err}"))?;

    let mut tracks_by_id = HashMap::with_capacity(fetched_tracks.len());
    for track in fetched_tracks {
        let mut mapped = model_track_to_core_queue_track(&track);
        if let Some(existing) = existing_by_id.get(&track.id) {
            preserve_existing_catalog_metadata(&mut mapped, existing);
        }
        tracks_by_id.insert(track.id, mapped);
    }

    let mut queue_tracks = Vec::with_capacity(queue_state.queue_items.len());
    for item in &queue_state.queue_items {
        if let Some(queue_track) = tracks_by_id.get(&item.track_id) {
            queue_tracks.push(queue_track.clone());
            continue;
        }

        match engine.get_track(item.track_id).await {
            Ok(track) => {
                let mut mapped = model_track_to_core_queue_track(&track);
                if let Some(existing) = existing_by_id.get(&track.id) {
                    preserve_existing_catalog_metadata(&mut mapped, existing);
                }
                tracks_by_id.insert(item.track_id, mapped.clone());
                queue_tracks.push(mapped);
            }
            Err(err) => {
                log::warn!(
                    "[QConnect] Unable to hydrate remote queue track {}: {}",
                    item.track_id,
                    err
                );
            }
        }
    }

    if queue_tracks.is_empty() {
        return Err("remote queue materialization produced zero playable tracks".to_string());
    }

    // Resolve start index from remote state first, then from the local playback
    // cursor only if that track is still part of the remote queue.
    let current_playback_track_id = match engine.get_playback_state().track_id {
        0 => None,
        track_id => Some(track_id),
    };
    let mut start_index =
        resolve_remote_start_index(queue_state, renderer_queue_item_id, renderer_track_id);
    if start_index.is_none() {
        start_index = resolve_remote_start_index(
            queue_state,
            renderer_next_queue_item_id,
            renderer_next_track_id,
        )
        .map(|index| index.saturating_sub(1));
    }
    if start_index.is_none() {
        start_index = current_playback_track_id.and_then(|track_id| {
            queue_state
                .queue_items
                .iter()
                .position(|item| item.track_id == track_id)
        });
    }
    if start_index.is_none() && !queue_tracks.is_empty() {
        start_index = Some(0);
    }
    let core_shuffle_order = resolve_core_shuffle_order(queue_state);
    // A queue update is usable only once it contains a WS-authored order,
    // either copied from QueueState.shuffled_track_indexes or reproduced from
    // the seed/pivot carried by the incremental queue event.
    let effective_shuffle_enabled = queue_state.shuffle_mode && core_shuffle_order.is_some();
    log::info!(
        "[QConnect] materialize_remote_queue: setting queue with {} tracks, start_index={:?}, local_track_id={:?}, remote_shuffle_mode={}, shuffle_order_present={}, engine_shuffle_enabled={}",
        queue_tracks.len(),
        start_index,
        current_playback_track_id,
        queue_state.shuffle_mode,
        core_shuffle_order.is_some(),
        effective_shuffle_enabled,
    );
    engine
        .set_queue_with_order(
            queue_tracks,
            start_index,
            effective_shuffle_enabled,
            core_shuffle_order.clone(),
        )
        .await;

    {
        let mut state = sync_state.lock().await;
        state.last_materialized_start_index = start_index;
        state.last_materialized_core_shuffle_order = core_shuffle_order;
    }

    let local_track_missing_from_remote = current_playback_track_id
        .map(|track_id| {
            !queue_state
                .queue_items
                .iter()
                .any(|item| item.track_id == track_id)
        })
        .unwrap_or(true);

    if let Some(index) = start_index {
        if local_track_missing_from_remote {
            log::info!(
                "[QConnect] materialize_remote_queue: aligning queue cursor to remote index {}",
                index
            );
            let _ = engine.play_index(index).await;
        }
    }

    if current_playback_track_id.is_some()
        && current_playback_track_id != renderer_track_id
        && local_track_missing_from_remote
        && matches!(
            renderer_playing_state,
            Some(PLAYING_STATE_STOPPED | PLAYING_STATE_UNKNOWN)
        )
    {
        log::info!(
            "[QConnect] materialize_remote_queue: stopping stale local playback track {:?} after remote queue replacement",
            current_playback_track_id
        );
        let _ = engine.stop();
    }

    Ok(true)
}

async fn align_remote_occurrence(
    engine: &impl QconnectRendererEngine,
    sync: &Arc<Mutex<QconnectRemoteSyncState>>,
    target: &qconnect_core::QueueItem,
) -> Result<bool, String> {
    let queue = sync.lock().await.last_remote_queue_state.clone();
    let Some(queue) = queue else {
        let (_, before) = engine.get_all_queue_tracks().await;
        align_queue_cursor(engine, target.track_id).await?;
        let (tracks, _) = engine.get_all_queue_tracks().await;
        return Ok(before != tracks.iter().position(|track| track.id == target.track_id));
    };
    let index = queue.queue_items.iter().chain(queue.autoplay_items.iter())
        .position(|item| item.queue_item_id == target.queue_item_id && item.track_id == target.track_id)
        .ok_or("renderer occurrence is absent from synchronized queue")?;
    let (tracks, current) = engine.get_all_queue_tracks().await;
    let cloud_ids: Vec<_> = queue.queue_items.iter().chain(queue.autoplay_items.iter()).map(|item| item.track_id).collect();
    if tracks.len() != cloud_ids.len() || !tracks.iter().zip(cloud_ids).all(|(track, id)| track.id == id && qconnect_queue_track_is_resolvable(track)) {
        return Err("local queue does not match synchronized occurrences".into());
    }
    let renderer = QConnectRendererState { current_track: Some(target.clone()), ..Default::default() };
    let projected = sync_remote_shuffle_projection(engine, sync, &queue, &renderer).await?;
    if !projected && current != Some(index) { engine.play_index(index).await; }
    Ok(current != Some(index))
}

pub async fn align_queue_cursor(
    engine: &impl QconnectRendererEngine,
    track_id: u64,
) -> Result<(), String> {
    let (tracks, current_index) = engine.get_all_queue_tracks().await;
    log::info!(
        "[QConnect] align_queue_cursor: track_id={track_id} queue_len={} current_index={:?}",
        tracks.len(),
        current_index
    );
    let matches: Vec<_> = tracks.iter().enumerate()
        .filter(|(_, track)| track.id == track_id && qconnect_queue_track_is_resolvable(track))
        .map(|(index, _)| index).collect();
    if let [target_index] = matches.as_slice() {
        if current_index != Some(*target_index) { engine.play_index(*target_index).await; }
        return Ok(());
    }
    Err("renderer needs an authoritative queue occurrence before moving the cursor".into())
}

// Recording-engine tests exercise ordering and admission; real writer behavior
// is covered separately in qbz-player.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    use async_trait::async_trait;
    use qbz_models::{Quality, QueueTrack, RepeatMode, Track};
    use qbz_player::PlaybackState;
    use qconnect_core::{QueueItem, QueueVersion};
    use tokio::sync::Mutex;

    use crate::renderer_engine::QconnectRendererEngine;
    use crate::{
        QConnectQueueState, QConnectRendererState, QconnectRemoteSyncState, RendererCommand,
    };

    #[derive(Default)]
    struct MockCalls {
        resumes: u32,
        pauses: u32,
        stops: u32,
        seeks: Vec<u64>,
        set_volumes: Vec<f32>,
        set_repeat_modes: u32,
        set_shuffles: Vec<bool>,
        set_queue_with_order: Vec<(bool, Option<Vec<usize>>)>,
        materialized_tracks: Vec<QueueTrack>,
        set_queues: u32,
        clear_queues: Vec<bool>,
        play_indexes: Vec<usize>,
        get_tracks_batch: u32,
        start_track_streams: Vec<u64>,
        start_positions: Vec<u64>,
        initial_playing: Vec<bool>,
    }

    /// Records every engine call; serves canned `PlaybackState` + queue snapshot.
    struct MockEngine {
        calls: Arc<StdMutex<MockCalls>>,
        playback: PlaybackState,
        queue_tracks: Vec<QueueTrack>,
        queue_index: Option<usize>,
        loaded_audio: bool,
        fail_load: bool,
    }

    impl MockEngine {
        fn new() -> Self {
            Self {
                calls: Arc::new(StdMutex::new(MockCalls::default())),
                playback: PlaybackState::default(),
                queue_tracks: Vec::new(),
                queue_index: None,
                loaded_audio: false,
                fail_load: false,
            }
        }

        fn calls(&self) -> std::sync::MutexGuard<'_, MockCalls> {
            self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl QconnectRendererEngine for MockEngine {
        fn resume(&self) -> Result<(), String> {
            self.calls().resumes += 1;
            Ok(())
        }
        fn pause(&self) -> Result<(), String> {
            self.calls().pauses += 1;
            Ok(())
        }
        fn stop(&self) -> Result<(), String> {
            self.calls().stops += 1;
            Ok(())
        }
        fn seek(&self, position_secs: u64) -> Result<(), String> {
            self.calls().seeks.push(position_secs);
            Ok(())
        }
        fn set_volume(&self, fraction: f32) -> Result<(), String> {
            self.calls().set_volumes.push(fraction);
            Ok(())
        }
        fn get_playback_state(&self) -> PlaybackState {
            self.playback.clone()
        }
        fn has_loaded_audio(&self) -> bool {
            self.loaded_audio
        }
        async fn set_repeat_mode(&self, _mode: RepeatMode) {
            self.calls().set_repeat_modes += 1;
        }
        async fn set_shuffle(&self, enabled: bool) {
            self.calls().set_shuffles.push(enabled);
        }
        async fn get_all_queue_tracks(&self) -> (Vec<QueueTrack>, Option<usize>) {
            (self.queue_tracks.clone(), self.queue_index)
        }
        async fn set_queue(&self, _tracks: Vec<QueueTrack>, _start_index: Option<usize>) {
            self.calls().set_queues += 1;
        }
        async fn set_queue_with_order(
            &self,
            tracks: Vec<QueueTrack>,
            _start_index: Option<usize>,
            shuffle_enabled: bool,
            shuffle_order: Option<Vec<usize>>,
        ) {
            let mut calls = self.calls();
            calls.materialized_tracks = tracks;
            calls
                .set_queue_with_order
                .push((shuffle_enabled, shuffle_order));
        }
        async fn clear_queue(&self, keep_current: bool) {
            self.calls().clear_queues.push(keep_current);
        }
        async fn play_index(&self, index: usize) -> Option<QueueTrack> {
            self.calls().play_indexes.push(index);
            None
        }
        async fn get_track(&self, track_id: u64) -> Result<Track, String> {
            Ok(mock_track(track_id))
        }
        async fn get_tracks_batch(&self, track_ids: &[u64]) -> Result<Vec<Track>, String> {
            self.calls().get_tracks_batch += 1;
            Ok(track_ids.iter().map(|&id| mock_track(id)).collect())
        }
        async fn start_track_stream(
            &self,
            track_id: u64,
            _quality: Quality,
            _duration_secs: u64,
            start_position_secs: u64,
        ) -> Result<(), String> {
            let mut calls = self.calls();
            calls.start_track_streams.push(track_id);
            calls.start_positions.push(start_position_secs);
            Ok(())
        }
        async fn start_track_stream_with_state(
            &self, track_id: u64, quality: Quality, duration: u64, position: u64, playing: bool,
        ) -> Result<(), String> {
            self.calls().initial_playing.push(playing);
            if self.fail_load { return Err("injected load failure".into()); }
            self.start_track_stream(track_id, quality, duration, position).await
        }
        fn current_output_format(&self) -> Option<(u32, u32)> {
            Some((44_100, 16))
        }
    }

    fn qi(track_id: u64, queue_item_id: u64) -> QueueItem {
        QueueItem {
            track_context_uuid: "ctx".to_string(),
            track_id,
            queue_item_id,
        }
    }

    fn mock_track(id: u64) -> Track {
        serde_json::from_value(serde_json::json!({ "id": id, "title": "t", "duration": 100 }))
            .expect("mock track")
    }

    fn mock_queue_track(id: u64) -> QueueTrack {
        model_track_to_core_queue_track(&mock_track(id))
    }

    #[tokio::test]
    async fn failed_load_is_retryable_and_does_not_resume_or_seek() {
        let mut engine = MockEngine::new();
        engine.fail_load = true;
        engine.queue_tracks = vec![mock_queue_track(7)];
        let sync = sync();
        let command = RendererCommand::SetState { playing_state: Some(PLAYING_STATE_PLAYING),
            current_position_ms: Some(45_000), current_track: Some(qi(7, 0)), next_track: None };
        assert!(apply_renderer_command(&engine, &sync, &command, &QConnectRendererState::default()).await.is_err());
        assert!(sync.lock().await.last_load_attempt.is_none());
        assert_eq!(engine.calls().resumes, 0);
        assert!(engine.calls().seeks.is_empty());
        engine.fail_load = false;
        apply_renderer_command(&engine, &sync, &command, &QConnectRendererState::default()).await.unwrap();
        assert_eq!(engine.calls().start_positions, vec![45]);
    }

    #[tokio::test]
    async fn handoff_cold_paused_track_is_prepared_paused_at_requested_position() {
        let mut engine = MockEngine::new();
        engine.playback.track_id = 7;
        engine.queue_tracks = vec![mock_queue_track(7)];
        let command = RendererCommand::SetState { playing_state: Some(PLAYING_STATE_PAUSED),
            current_position_ms: Some(45_000), current_track: Some(qi(7, 0)), next_track: None };
        apply_renderer_command(&engine, &sync(), &command, &QConnectRendererState::default()).await.unwrap();
        assert_eq!(engine.calls().start_positions, vec![45]);
        assert_eq!(engine.calls().initial_playing, vec![false]);
        assert_eq!(engine.calls().resumes, 0);
        assert!(engine.calls().seeks.is_empty());
    }

    #[tokio::test]
    async fn handoff_partial_target_and_pause_in_either_order_prepare_the_new_occurrence() {
        for target_first in [true, false] {
            let mut engine = MockEngine::new();
            engine.loaded_audio = true;
            engine.playback.track_id = 7;
            engine.queue_tracks = vec![mock_queue_track(7), mock_queue_track(8), mock_queue_track(7)];
            engine.queue_index = Some(0);
            let sync = sync();
            sync.lock().await.last_remote_queue_state = Some(QConnectQueueState {
                queue_items: vec![qi(7, 10), qi(8, 11), qi(7, 0)], ..Default::default() });
            let target = RendererCommand::SetState { playing_state: None,
                current_position_ms: None, current_track: Some(qi(7, 0)), next_track: None };
            let pause = RendererCommand::SetState { playing_state: Some(PLAYING_STATE_PAUSED),
                current_position_ms: Some(45_000), current_track: None, next_track: None };
            let commands = if target_first { [target, pause] } else { [pause, target] };
            let mut state = QConnectRendererState::default();
            for command in commands {
                qconnect_core::apply_renderer_command(&mut state, &command, 1);
                apply_renderer_command(&engine, &sync, &command, &state).await.unwrap();
            }
            assert_eq!(engine.calls().start_track_streams, vec![7]);
            assert_eq!(engine.calls().initial_playing, vec![false]);
            assert_eq!(engine.calls().start_positions, vec![45]);
            assert_eq!(engine.calls().play_indexes, vec![2]);
            assert_eq!(engine.calls().resumes, 0);
        }
    }

    #[tokio::test]
    async fn handoff_immediate_pause_never_seeks_to_retained_position() {
        let mut engine = MockEngine::new();
        engine.loaded_audio = true;
        engine.playback.track_id = 7;
        engine.playback.position = 168;
        engine.playback.is_playing = true;
        let sync = sync();
        sync.lock().await.last_load_attempt = Some((7, Instant::now()));
        let state = QConnectRendererState { current_position_ms: Some(0), current_track: Some(qi(7, 0)),
            playing_state: Some(PLAYING_STATE_PAUSED), ..Default::default() };
        let command = RendererCommand::SetState { playing_state: Some(PLAYING_STATE_PAUSED),
            current_position_ms: None, current_track: None, next_track: None };
        apply_renderer_command(&engine, &sync, &command, &state).await.unwrap();
        assert_eq!(engine.calls().pauses, 1);
        assert!(engine.calls().seeks.is_empty());
        assert!(engine.calls().play_indexes.is_empty());
        assert!(engine.calls().start_track_streams.is_empty());
    }

    #[tokio::test]
    async fn handoff_repeated_track_uses_occurrence_zero_without_replacing_queue() {
        let mut engine = MockEngine::new();
        engine.loaded_audio = true;
        engine.playback.track_id = 7;
        engine.queue_tracks = vec![mock_queue_track(7), mock_queue_track(8), mock_queue_track(7)];
        engine.queue_index = Some(0);
        let sync = sync();
        sync.lock().await.last_remote_queue_state = Some(QConnectQueueState {
            queue_items: vec![qi(7, 10), qi(8, 11), qi(7, 0)], ..Default::default() });
        let command = RendererCommand::SetState { playing_state: Some(PLAYING_STATE_PAUSED),
            current_position_ms: None, current_track: Some(qi(7, 0)), next_track: None };
        apply_renderer_command(&engine, &sync, &command, &QConnectRendererState::default()).await.unwrap();
        assert_eq!(engine.calls().play_indexes, vec![2]);
        assert_eq!(engine.calls().set_queues, 0);
    }

    #[test]
    fn qconnect_admission_uses_provenance_and_accepts_materialized_rows() {
        for source in [
            "qobuz",
            "qobuz_download",
            "qobuz_purchase",
            "offline",
            LEGACY_QCONNECT_REMOTE_QUEUE_SOURCE,
        ] {
            let mut track = mock_queue_track(7);
            track.source = Some(source.to_string());
            track.is_local = source == "qobuz_download";
            assert!(qconnect_queue_track_is_resolvable(&track), "{source}");
        }

        for source in ["local", "plex", "jellyfin", "subsonic", "navidrome"] {
            let mut track = mock_queue_track(7);
            track.source = Some(source.to_string());
            track.is_local = true;
            assert!(!qconnect_queue_track_is_resolvable(&track), "{source}");
        }
    }

    #[test]
    fn qconnect_admission_legacy_rows_require_non_local_positive_ids() {
        let mut track = mock_queue_track(7);
        track.source = None;
        track.is_local = false;
        assert!(qconnect_queue_track_is_resolvable(&track));

        track.is_local = true;
        assert!(!qconnect_queue_track_is_resolvable(&track));
        track.id = 0;
        track.is_local = false;
        assert!(!qconnect_queue_track_is_resolvable(&track));
    }

    fn queue_state(
        version: QueueVersion,
        items: Vec<QueueItem>,
        shuffle_mode: bool,
        shuffle_order: Option<Vec<usize>>,
    ) -> QConnectQueueState {
        QConnectQueueState {
            version,
            queue_items: items,
            shuffle_mode,
            shuffle_order,
            autoplay_mode: false,
            autoplay_loading: false,
            autoplay_items: Vec::new(),
            updated_at_ms: 0,
            last_server_queue_hash: None,
        }
    }

    fn sync() -> Arc<Mutex<QconnectRemoteSyncState>> {
        Arc::new(Mutex::new(QconnectRemoteSyncState::default()))
    }

    /// A queue created from `/album/get` carries both edition suffixes. The
    /// id-only QConnect echo must not replace them with the terse batch-track
    /// response, because every metadata consumer reads the materialized core
    /// queue after this point.
    #[tokio::test]
    async fn materialize_preserves_rich_metadata_from_matching_local_queue() {
        let mut engine = MockEngine::new();
        let mut rich = mock_queue_track(7);
        rich.title = "What Is Life".to_string();
        rich.version = Some("Backing Track / Bonus Track".to_string());
        rich.artist = "George Harrison".to_string();
        rich.album = "All Things Must Pass".to_string();
        rich.album_version = Some("Remastered 2014".to_string());
        rich.artwork_url = Some("https://example.test/cover.jpg".to_string());
        rich.album_id = Some("album-7".to_string());
        rich.artist_id = Some(42);
        rich.bit_depth = Some(24);
        rich.sample_rate = Some(96_000.0);
        rich.hires = true;
        rich.source = Some("qobuz".to_string());
        rich.context_kind = Some("album".to_string());
        rich.context_id = Some("album-7".to_string());
        engine.queue_tracks = vec![rich];
        engine.queue_index = Some(0);

        materialize_remote_queue(
            &engine,
            &sync(),
            &queue_state(QueueVersion::new(1, 0), vec![qi(7, 0)], false, None),
        )
        .await
        .unwrap();

        let calls = engine.calls();
        let track = calls
            .materialized_tracks
            .first()
            .expect("materialized track");
        assert_eq!(
            track.version.as_deref(),
            Some("Backing Track / Bonus Track")
        );
        assert_eq!(track.album_version.as_deref(), Some("Remastered 2014"));
        assert_eq!(track.artist, "George Harrison");
        assert_eq!(track.album, "All Things Must Pass");
        assert_eq!(track.album_id.as_deref(), Some("album-7"));
        assert_eq!(track.artist_id, Some(42));
        assert_eq!(track.bit_depth, Some(24));
        assert_eq!(track.sample_rate, Some(96_000.0));
        assert!(track.hires);
        assert_eq!(track.source.as_deref(), Some("qobuz"));
        assert!(
            track.context_kind.is_none(),
            "remote context remains authoritative"
        );
        assert!(
            track.context_id.is_none(),
            "remote context remains authoritative"
        );
    }

    /// #2 — two loads for the same track within the dedup window trigger exactly
    /// one `start_track_stream`; the second is swallowed by the 5s window even
    /// though the audio thread hasn't reported the track yet.
    #[tokio::test]
    async fn ensure_remote_track_loaded_dedups_within_window() {
        let engine = MockEngine::new(); // playback track_id 0 != 42 → would reload
        let sync = sync();
        ensure_remote_track_loaded(&engine, &sync, 42, None, 0)
            .await
            .unwrap();
        ensure_remote_track_loaded(&engine, &sync, 42, None, 0)
            .await
            .unwrap();
        assert_eq!(engine.calls().start_track_streams, vec![42]);
    }

    /// #2 — no reload when the audio thread already plays the requested track.
    #[tokio::test]
    async fn ensure_remote_track_loaded_skips_when_track_unchanged() {
        let mut engine = MockEngine::new();
        engine.playback = PlaybackState {
            track_id: 42,
            ..Default::default()
        };
        engine.loaded_audio = true;
        let sync = sync();
        ensure_remote_track_loaded(&engine, &sync, 42, None, 0)
            .await
            .unwrap();
        assert!(engine.calls().start_track_streams.is_empty());
    }

    /// #1 / #387 — a SetState targeting the SAME track at <=1s while local is well
    /// ahead is a cloud echo: the seek is rejected.
    #[tokio::test]
    async fn explicit_zero_seek_is_honored_without_evidence_of_echo() {
        let mut engine = MockEngine::new();
        engine.playback = PlaybackState {
            track_id: 7,
            position: 30,
            ..Default::default()
        };
        engine.queue_tracks = vec![mock_queue_track(7)];
        engine.queue_index = Some(0);
        engine.loaded_audio = true;
        let sync = sync();
        let cmd = RendererCommand::SetState {
            playing_state: None,
            current_position_ms: Some(0),
            current_track: Some(qi(7, 0)),
            next_track: None,
        };
        apply_renderer_command(&engine, &sync, &cmd, &QConnectRendererState::default())
            .await
            .unwrap();
        assert_eq!(engine.calls().seeks, vec![0]);
    }

    #[tokio::test]
    async fn explicit_seek_does_not_reload_an_already_playing_stream() {
        let mut engine = MockEngine::new();
        engine.playback = PlaybackState {
            track_id: 7,
            position: 67,
            is_playing: true,
            ..Default::default()
        };
        engine.loaded_audio = true;
        engine.queue_tracks = vec![mock_queue_track(7)];
        engine.queue_index = Some(0);
        let command = RendererCommand::SetState {
            playing_state: Some(PLAYING_STATE_PLAYING),
            current_position_ms: Some(0),
            current_track: Some(qi(7, 0)),
            next_track: None,
        };
        apply_renderer_command(
            &engine,
            &sync(),
            &command,
            &QConnectRendererState::default(),
        )
        .await
        .unwrap();
        assert_eq!(engine.calls().resumes, 0);
        assert_eq!(engine.calls().seeks, vec![0]);
        assert!(engine.calls().start_track_streams.is_empty());
    }

    /// #1 / #387 — a genuine peer seek (target far from local) IS honored, even
    /// for the same track (the bug the all-or-nothing peer gate caused).
    #[tokio::test]
    async fn apply_renderer_command_honors_genuine_seek() {
        let mut engine = MockEngine::new();
        engine.playback = PlaybackState {
            track_id: 7,
            position: 10,
            ..Default::default()
        };
        engine.queue_tracks = vec![mock_queue_track(7)];
        engine.queue_index = Some(0);
        engine.loaded_audio = true;
        let sync = sync();
        let cmd = RendererCommand::SetState {
            playing_state: None,
            current_position_ms: Some(40_000),
            current_track: Some(qi(7, 0)),
            next_track: None,
        };
        apply_renderer_command(&engine, &sync, &cmd, &QConnectRendererState::default())
            .await
            .unwrap();
        assert_eq!(engine.calls().seeks, vec![40]);
    }

    /// A state-only resume (current_track = null) on a COLD engine — queue
    /// cursor restored but no audio loaded, as after a daemon restart — must
    /// cold-load the cloud's current track at the handed-off position instead
    /// of issuing a bare resume that dies in the audio thread ("cannot resume
    /// - no audio data available") and wedges the controller at paused 0:00.
    #[tokio::test]
    async fn apply_renderer_command_cold_resume_loads_current_track() {
        let mut engine = MockEngine::new();
        // Cursor reports the restored track, but nothing is loaded: the plain
        // track-id guard would skip, which is why the force path is used.
        engine.playback = PlaybackState {
            track_id: 7,
            ..Default::default()
        };
        engine.queue_tracks = vec![mock_queue_track(7)];
        engine.queue_index = Some(0);
        engine.loaded_audio = false;
        let sync = sync();
        let cmd = RendererCommand::SetState {
            playing_state: Some(PLAYING_STATE_PLAYING),
            current_position_ms: None,
            current_track: None,
            next_track: None,
        };
        // The cloud's view carries the session's current track + position.
        let renderer_state = QConnectRendererState {
            current_track: Some(qi(7, 2)),
            current_position_ms: Some(242_491),
            ..Default::default()
        };
        apply_renderer_command(&engine, &sync, &cmd, &renderer_state)
            .await
            .unwrap();
        let calls = engine.calls();
        assert_eq!(
            calls.start_track_streams,
            vec![7],
            "cold resume must load the session's current track"
        );
        assert_eq!(
            calls.start_positions,
            vec![242],
            "load must resume at the handed-off position"
        );
        assert_eq!(calls.resumes, 0, "no bare resume on a cold engine");
    }

    /// The field shape from the owner's 2026-08-31 regression: both the
    /// command and renderer projection omit current_track, but the synchronized
    /// queue still has the authoritative cursor. This must cold-load that row,
    /// never fall through to `resume()` on an empty engine.
    #[tokio::test]
    async fn cold_resume_waits_for_fresh_target_instead_of_retained_queue() {
        let mut engine = MockEngine::new();
        engine.playback = PlaybackState {
            track_id: 7,
            position: 38,
            ..Default::default()
        };
        engine.queue_tracks = vec![mock_queue_track(6), mock_queue_track(7)];
        engine.queue_index = Some(1);
        engine.loaded_audio = false;
        let sync = sync();
        let cmd = RendererCommand::SetState {
            playing_state: Some(PLAYING_STATE_PLAYING),
            current_position_ms: None,
            current_track: None,
            next_track: None,
        };

        apply_renderer_command(&engine, &sync, &cmd, &QConnectRendererState::default())
            .await
            .expect("partial intent is accepted while awaiting a fresh occurrence");

        let calls = engine.calls();
        assert!(calls.start_track_streams.is_empty());
        assert!(calls.start_positions.is_empty());
        assert_eq!(calls.resumes, 0);
    }

    #[tokio::test]
    async fn cold_resume_never_guesses_a_local_queue_id_is_qobuz() {
        let mut engine = MockEngine::new();
        engine.playback = PlaybackState {
            track_id: 7,
            ..Default::default()
        };
        let mut local = mock_queue_track(7);
        local.source = Some("local".to_string());
        local.is_local = true;
        engine.queue_tracks = vec![local];
        engine.queue_index = Some(0);
        engine.loaded_audio = false;
        let cmd = RendererCommand::SetState {
            playing_state: Some(PLAYING_STATE_PLAYING),
            current_position_ms: None,
            current_track: None,
            next_track: None,
        };

        apply_renderer_command(&engine, &sync(), &cmd, &QConnectRendererState::default())
            .await.expect("partial intent can wait without resolving a local id");

        let calls = engine.calls();
        assert!(calls.start_track_streams.is_empty());
        assert_eq!(calls.resumes, 0);
    }

    /// The cold-start load never fires while audio is loaded: a resume during
    /// live playback (or a cloud echo) stays a plain resume, no re-stream.
    #[tokio::test]
    async fn apply_renderer_command_warm_resume_stays_plain() {
        let mut engine = MockEngine::new();
        engine.playback = PlaybackState {
            track_id: 7,
            position: 30,
            ..Default::default()
        };
        engine.queue_tracks = vec![mock_queue_track(7)];
        engine.queue_index = Some(0);
        engine.loaded_audio = true;
        let sync = sync();
        let cmd = RendererCommand::SetState {
            playing_state: Some(PLAYING_STATE_PLAYING),
            current_position_ms: None,
            current_track: None,
            next_track: None,
        };
        let renderer_state = QConnectRendererState {
            current_track: Some(qi(7, 2)),
            ..Default::default()
        };
        apply_renderer_command(&engine, &sync, &cmd, &renderer_state)
            .await
            .unwrap();
        let calls = engine.calls();
        assert!(
            calls.start_track_streams.is_empty(),
            "no re-stream while audio is loaded"
        );
        assert_eq!(calls.resumes, 1, "plain resume on a warm engine");
    }

    /// WS-authoritative shuffle: a standalone renderer command has no seed/order
    /// and therefore must not mutate the core queue at all. The queue event is
    /// the sole authority.
    #[tokio::test]
    async fn apply_renderer_command_setshufflemode_does_not_touch_local_queue() {
        let mut engine = MockEngine::new();
        engine.queue_tracks = vec![
            mock_queue_track(1),
            mock_queue_track(2),
            mock_queue_track(3),
        ];
        engine.queue_index = Some(0);
        let sync = sync();
        let cmd = RendererCommand::SetShuffleMode { shuffle_mode: true };
        apply_renderer_command(&engine, &sync, &cmd, &QConnectRendererState::default())
            .await
            .unwrap();
        let calls = engine.calls();
        assert!(
            calls.set_shuffles.is_empty(),
            "renderer command must not call local shuffle"
        );
        assert!(
            calls.set_queue_with_order.is_empty(),
            "renderer command must not apply an order without queue authority"
        );
    }

    /// #3 — a state-only SetState (command.current_track = None) must NOT align the
    /// cursor or load a track from the renderer_state's stale current_track (the
    /// iOS-pause-jumped-back fix); the playing_state is still applied.
    #[tokio::test]
    async fn apply_renderer_command_skips_track_ops_on_state_only_update() {
        let mut engine = MockEngine::new();
        engine.playback = PlaybackState {
            track_id: 7,
            position: 5,
            ..Default::default()
        };
        engine.loaded_audio = true;
        let sync = sync();
        let cmd = RendererCommand::SetState {
            playing_state: Some(PLAYING_STATE_PAUSED),
            current_position_ms: None,
            current_track: None,
            next_track: None,
        };
        // renderer_state carries a STALE current_track the projection would fall
        // back to — it must not drive a load/align.
        let renderer_state = QConnectRendererState {
            current_track: Some(qi(99, 0)),
            ..Default::default()
        };
        apply_renderer_command(&engine, &sync, &cmd, &renderer_state)
            .await
            .unwrap();
        let calls = engine.calls();
        assert!(
            calls.start_track_streams.is_empty(),
            "no load on state-only update"
        );
        assert!(
            calls.play_indexes.is_empty(),
            "no cursor align on state-only update"
        );
        assert_eq!(calls.pauses, 1, "pause still applied");
    }

    /// #5 — shuffle deferral: the first event (shuffle_mode=true, order=None)
    /// materializes with shuffle_enabled=false (no invented identity order); the
    /// second event (authoritative order present) enables shuffle.
    #[tokio::test]
    async fn materialize_defers_shuffle_until_authoritative_order() {
        let engine = MockEngine::new();
        let sync = sync();
        let items = vec![qi(10, 0), qi(11, 1)];

        let q1 = queue_state(QueueVersion::new(1, 0), items.clone(), true, None);
        materialize_remote_queue(&engine, &sync, &q1).await.unwrap();
        {
            let calls = engine.calls();
            assert_eq!(calls.set_queue_with_order.len(), 1);
            assert!(
                !calls.set_queue_with_order[0].0,
                "shuffle deferred while order absent"
            );
        }

        let q2 = queue_state(QueueVersion::new(1, 1), items, true, Some(vec![1, 0]));
        materialize_remote_queue(&engine, &sync, &q2).await.unwrap();
        {
            let calls = engine.calls();
            assert_eq!(calls.set_queue_with_order.len(), 2);
            assert!(
                calls.set_queue_with_order[1].0,
                "shuffle enabled once authoritative order present"
            );
        }
    }

    /// Activating ownership alone never authorizes a retained audio target.
    #[tokio::test]
    async fn set_active_waits_for_fresh_state_when_audio_torn_down() {
        let mut engine = MockEngine::new();
        engine.playback = PlaybackState {
            track_id: 7, // stale id left by stop(); audio is gone
            ..Default::default()
        };
        engine.loaded_audio = false;
        let sync = sync();
        let cmd = RendererCommand::SetActive { active: true };
        let renderer_state = QConnectRendererState {
            current_track: Some(qi(7, 0)),
            current_position_ms: Some(45_000),
            ..Default::default()
        };
        apply_renderer_command(&engine, &sync, &cmd, &renderer_state)
            .await
            .unwrap();
        let calls = engine.calls();
        assert!(calls.start_track_streams.is_empty());
        assert!(calls.start_positions.is_empty());
        assert_eq!(calls.resumes, 0);
    }

    #[tokio::test]
    async fn set_active_false_waits_for_topology_before_stopping_audio() {
        let mut engine = MockEngine::new();
        engine.playback = PlaybackState {
            track_id: 7,
            is_playing: true,
            ..Default::default()
        };
        engine.loaded_audio = true;

        apply_renderer_command(
            &engine,
            &sync(),
            &RendererCommand::SetActive { active: false },
            &QConnectRendererState::default(),
        )
        .await
        .unwrap();

        assert_eq!(
            engine.calls().stops,
            0,
            "SetActive(false) must not synthesize a paused edge before ACTIVE_RENDERER_CHANGED"
        );
    }

    /// #1 (no-interrupt) — a SetActive(true) while the renderer is ALREADY
    /// streaming this exact track with audio loaded must NOT restart it (guards
    /// against a spurious activation tearing down live playback).
    #[tokio::test]
    async fn set_active_does_not_restart_when_already_streaming() {
        let mut engine = MockEngine::new();
        engine.playback = PlaybackState {
            track_id: 7,
            ..Default::default()
        };
        engine.loaded_audio = true; // live playback in progress
        let sync = sync();
        let cmd = RendererCommand::SetActive { active: true };
        let renderer_state = QConnectRendererState {
            current_track: Some(qi(7, 0)),
            current_position_ms: Some(45_000),
            ..Default::default()
        };
        apply_renderer_command(&engine, &sync, &cmd, &renderer_state)
            .await
            .unwrap();
        assert!(
            engine.calls().start_track_streams.is_empty(),
            "must not restart an already-streaming track on a spurious SetActive"
        );
    }

    /// Regression (2026-09-01): local track 7 was actively playing when a
    /// stale SetActive carried remote track 99. The command created a one-track
    /// queue and destroyed the local session before its queue upload settled.
    #[tokio::test]
    async fn set_active_does_not_replace_different_active_local_track() {
        let mut engine = MockEngine::new();
        engine.playback = PlaybackState {
            track_id: 7,
            is_playing: true,
            ..Default::default()
        };
        engine.loaded_audio = true;
        engine.queue_tracks = vec![mock_queue_track(7), mock_queue_track(8)];
        engine.queue_index = Some(0);

        let renderer_state = QConnectRendererState {
            current_track: Some(qi(99, 9)),
            current_position_ms: Some(71_000),
            ..Default::default()
        };
        apply_renderer_command(
            &engine,
            &sync(),
            &RendererCommand::SetActive { active: true },
            &renderer_state,
        )
        .await
        .unwrap();

        let calls = engine.calls();
        assert!(calls.start_track_streams.is_empty());
        assert_eq!(calls.set_queues, 0);
        assert!(calls.play_indexes.is_empty());
    }

    /// #1 (takeback first-load via SetState) — when the FIRST load on a takeback
    /// lands in the SetState path (SetActive arrived before current_track was
    /// known, so the force-stream couldn't fire), the load must stream at the
    /// cloud's reported position, not 0 — so a mid-track takeback resumes where
    /// the peer was instead of restarting (a forward seek past the buffered
    /// watermark is silently ignored, so streaming from 0 stuck at the start).
    #[tokio::test]
    async fn apply_renderer_command_setstate_streams_at_reported_position() {
        let mut engine = MockEngine::new(); // playback track_id 0 → fresh load
        engine.queue_tracks = vec![mock_queue_track(7)];
        let sync = sync();
        let cmd = RendererCommand::SetState {
            playing_state: Some(PLAYING_STATE_PLAYING),
            current_position_ms: Some(118_000),
            current_track: Some(qi(7, 1)),
            next_track: None,
        };
        apply_renderer_command(&engine, &sync, &cmd, &QConnectRendererState::default())
            .await
            .unwrap();
        let calls = engine.calls();
        assert_eq!(calls.start_track_streams, vec![7], "fresh takeback load");
        assert_eq!(
            calls.start_positions,
            vec![118],
            "takeback load must resume at the cloud position (118s), not 0"
        );
        assert_eq!(
            calls.seeks,
            Vec::<u64>::new(),
            "the load already starts at 118s; the same SetState must not seek again"
        );
    }
}
