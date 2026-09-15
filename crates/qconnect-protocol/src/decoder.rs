use prost::Message;
use qconnect_core::QueueVersion;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    queue_command_proto::{
        AutoplayModeSetMessage, AutoplayTracksLoadedMessage, AutoplayTracksRemovedMessage,
        CtrlActiveRendererChangedMessage, CtrlAddRendererMessage,
        CtrlDeviceAudioQualityChangedMessage, CtrlFileAudioQualityChangedMessage,
        CtrlLoopModeSetMessage, CtrlMaxAudioQualityChangedMessage, CtrlRemoveRendererMessage,
        CtrlRendererStateUpdatedMessage, CtrlSessionStateMessage, CtrlUpdateRendererMessage,
        CtrlVolumeChangedMessage, CtrlVolumeMutedMessage, QConnectMessage, QConnectMessageType,
        QConnectMessages, QueueClearedMessage, QueueErrorMessage, QueueStateMessage, QueueTrack,
        QueueTrackWithContext, QueueTracksAddedFromAutoplayMessage, QueueTracksAddedMessage,
        QueueTracksInsertedMessage, QueueTracksLoadedMessage, QueueTracksRemovedMessage,
        QueueTracksReorderedMessage, QueueVersionRef, RendererMuteVolumeMessage,
        RendererSetActiveMessage, RendererSetLoopModeMessage, RendererSetMaxAudioQualityMessage,
        RendererSetShuffleModeMessage, RendererSetStateMessage, RendererSetVolumeMessage,
        ShuffleModeSetMessage,
    },
    ProtocolError, QueueEventType, QueueServerEvent, RendererCommandType, RendererServerCommand,
};

pub fn decode_queue_server_events(payload: &[u8]) -> Result<Vec<QueueServerEvent>, ProtocolError> {
    let batch = QConnectMessages::decode(payload)?;
    let mut events = Vec::new();

    for message in batch.messages {
        if let Some(event) = decode_queue_server_event(message)? {
            events.push(event);
        }
    }

    Ok(events)
}

/// Decode the first renderer `PlaybackErrorMessage` from a batch, if any.
///
/// `playback_error` rides at tag 3 of each `QConnectMessage` inside the batch
/// `messages[]` (the official `Message` envelope record: message_type #1,
/// error #2, playback_error #3, oneof payloads 21-105). This is resolved from
/// the actual transport bytes: the decoder receives the batch `QConnectMessages`,
/// so the field lives on the per-message record, not on the batch wrapper.
pub fn decode_playback_error(payload: &[u8]) -> Option<crate::event::PlaybackErrorEvent> {
    let batch = QConnectMessages::decode(payload).ok()?;
    let msg = batch
        .messages
        .into_iter()
        .find_map(|message| message.playback_error)?;
    Some(crate::event::PlaybackErrorEvent {
        queue_item_id: i32_to_u64(msg.queue_item_id.unwrap_or(0)).ok()?,
        error_type: crate::event::ErrorType::from_wire(msg.error_type.unwrap_or(0)),
        queue_version: queue_version_opt(msg.queue_version).ok().flatten(),
    })
}

fn decode_queue_server_event(
    message: QConnectMessage,
) -> Result<Option<QueueServerEvent>, ProtocolError> {
    let message_type = resolve_queue_message_type(&message);
    let Some(message_type) = message_type else {
        return Ok(None);
    };

    let event = match message_type {
        code if code == QConnectMessageType::MessageTypeSrvrCtrlQueueErrorMessage as i32 => {
            let Some(payload) = message.srvr_ctrl_queue_error_message else {
                return Ok(None);
            };
            map_queue_error(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlQueueCleared as i32 => {
            let Some(payload) = message.srvr_ctrl_queue_cleared else {
                return Ok(None);
            };
            map_queue_cleared(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlQueueState as i32 => {
            let Some(payload) = message.srvr_ctrl_queue_state else {
                return Ok(None);
            };
            map_queue_state(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlQueueTracksLoaded as i32 => {
            let Some(payload) = message.srvr_ctrl_queue_tracks_loaded else {
                return Ok(None);
            };
            map_tracks_loaded(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlQueueTracksInserted as i32 => {
            let Some(payload) = message.srvr_ctrl_queue_tracks_inserted else {
                return Ok(None);
            };
            map_tracks_inserted(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlQueueTracksAdded as i32 => {
            let Some(payload) = message.srvr_ctrl_queue_tracks_added else {
                return Ok(None);
            };
            map_tracks_added(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlQueueTracksRemoved as i32 => {
            let Some(payload) = message.srvr_ctrl_queue_tracks_removed else {
                return Ok(None);
            };
            map_tracks_removed(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlQueueTracksReordered as i32 => {
            let Some(payload) = message.srvr_ctrl_queue_tracks_reordered else {
                return Ok(None);
            };
            map_tracks_reordered(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlShuffleModeSet as i32 => {
            let Some(payload) = message.srvr_ctrl_shuffle_mode_set else {
                return Ok(None);
            };
            map_shuffle_mode_set(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlAutoplayModeSet as i32 => {
            let Some(payload) = message.srvr_ctrl_autoplay_mode_set else {
                return Ok(None);
            };
            map_autoplay_mode_set(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlAutoplayTracksLoaded as i32 => {
            let Some(payload) = message.srvr_ctrl_autoplay_tracks_loaded else {
                return Ok(None);
            };
            map_autoplay_tracks_loaded(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlAutoplayTracksRemoved as i32 => {
            let Some(payload) = message.srvr_ctrl_autoplay_tracks_removed else {
                return Ok(None);
            };
            map_autoplay_tracks_removed(payload)?
        }
        code if code
            == QConnectMessageType::MessageTypeSrvrCtrlQueueTracksAddedFromAutoplay as i32 =>
        {
            let Some(payload) = message.srvr_ctrl_queue_tracks_added_from_autoplay else {
                return Ok(None);
            };
            map_tracks_added_from_autoplay(payload)?
        }
        // Session management events (81-87, 97-101)
        code if code == QConnectMessageType::MessageTypeSrvrCtrlSessionState as i32 => {
            let Some(payload) = message.srvr_ctrl_session_state else {
                return Ok(None);
            };
            map_session_state(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlRendererStateUpdated as i32 => {
            let Some(payload) = message.srvr_ctrl_renderer_state_updated else {
                return Ok(None);
            };
            map_ctrl_renderer_state_updated(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlAddRenderer as i32 => {
            let Some(payload) = message.srvr_ctrl_add_renderer else {
                return Ok(None);
            };
            map_add_renderer(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlUpdateRenderer as i32 => {
            let Some(payload) = message.srvr_ctrl_update_renderer else {
                return Ok(None);
            };
            map_update_renderer(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlRemoveRenderer as i32 => {
            let Some(payload) = message.srvr_ctrl_remove_renderer else {
                return Ok(None);
            };
            map_remove_renderer(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlActiveRendererChanged as i32 => {
            let Some(payload) = message.srvr_ctrl_active_renderer_changed else {
                return Ok(None);
            };
            map_active_renderer_changed(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlVolumeChanged as i32 => {
            let Some(payload) = message.srvr_ctrl_volume_changed else {
                return Ok(None);
            };
            map_ctrl_volume_changed(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlLoopModeSet as i32 => {
            let Some(payload) = message.srvr_ctrl_loop_mode_set else {
                return Ok(None);
            };
            map_ctrl_loop_mode_set(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlVolumeMuted as i32 => {
            let Some(payload) = message.srvr_ctrl_volume_muted else {
                return Ok(None);
            };
            map_ctrl_volume_muted(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlMaxAudioQualityChanged as i32 => {
            let Some(payload) = message.srvr_ctrl_max_audio_quality_changed else {
                return Ok(None);
            };
            map_ctrl_max_audio_quality_changed(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrCtrlFileAudioQualityChanged as i32 => {
            let Some(payload) = message.srvr_ctrl_file_audio_quality_changed else {
                return Ok(None);
            };
            map_ctrl_file_audio_quality_changed(payload)?
        }
        code if code
            == QConnectMessageType::MessageTypeSrvrCtrlDeviceAudioQualityChanged as i32 =>
        {
            let Some(payload) = message.srvr_ctrl_device_audio_quality_changed else {
                return Ok(None);
            };
            map_ctrl_device_audio_quality_changed(payload)?
        }
        _ => return Ok(None),
    };

    Ok(Some(event))
}

fn resolve_queue_message_type(message: &QConnectMessage) -> Option<i32> {
    message.message_type.or_else(|| {
        if message.srvr_ctrl_queue_error_message.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlQueueErrorMessage as i32);
        }
        if message.srvr_ctrl_queue_cleared.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlQueueCleared as i32);
        }
        if message.srvr_ctrl_queue_state.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlQueueState as i32);
        }
        if message.srvr_ctrl_queue_tracks_loaded.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlQueueTracksLoaded as i32);
        }
        if message.srvr_ctrl_queue_tracks_inserted.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlQueueTracksInserted as i32);
        }
        if message.srvr_ctrl_queue_tracks_added.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlQueueTracksAdded as i32);
        }
        if message.srvr_ctrl_queue_tracks_removed.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlQueueTracksRemoved as i32);
        }
        if message.srvr_ctrl_queue_tracks_reordered.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlQueueTracksReordered as i32);
        }
        if message.srvr_ctrl_shuffle_mode_set.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlShuffleModeSet as i32);
        }
        if message.srvr_ctrl_autoplay_mode_set.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlAutoplayModeSet as i32);
        }
        if message.srvr_ctrl_autoplay_tracks_loaded.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlAutoplayTracksLoaded as i32);
        }
        if message.srvr_ctrl_autoplay_tracks_removed.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlAutoplayTracksRemoved as i32);
        }
        if message.srvr_ctrl_queue_tracks_added_from_autoplay.is_some() {
            return Some(
                QConnectMessageType::MessageTypeSrvrCtrlQueueTracksAddedFromAutoplay as i32,
            );
        }
        // Session management
        if message.srvr_ctrl_session_state.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlSessionState as i32);
        }
        if message.srvr_ctrl_renderer_state_updated.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlRendererStateUpdated as i32);
        }
        if message.srvr_ctrl_add_renderer.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlAddRenderer as i32);
        }
        if message.srvr_ctrl_update_renderer.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlUpdateRenderer as i32);
        }
        if message.srvr_ctrl_remove_renderer.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlRemoveRenderer as i32);
        }
        if message.srvr_ctrl_active_renderer_changed.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlActiveRendererChanged as i32);
        }
        if message.srvr_ctrl_volume_changed.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlVolumeChanged as i32);
        }
        if message.srvr_ctrl_loop_mode_set.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlLoopModeSet as i32);
        }
        if message.srvr_ctrl_volume_muted.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlVolumeMuted as i32);
        }
        if message.srvr_ctrl_max_audio_quality_changed.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlMaxAudioQualityChanged as i32);
        }
        if message.srvr_ctrl_file_audio_quality_changed.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlFileAudioQualityChanged as i32);
        }
        if message.srvr_ctrl_device_audio_quality_changed.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrCtrlDeviceAudioQualityChanged as i32);
        }
        None
    })
}

pub fn decode_renderer_server_commands(
    payload: &[u8],
) -> Result<Vec<RendererServerCommand>, ProtocolError> {
    let batch = QConnectMessages::decode(payload)?;
    let mut commands = Vec::new();

    for message in batch.messages {
        if let Some(command) = decode_renderer_server_command(message)? {
            commands.push(command);
        }
    }

    Ok(commands)
}

fn decode_renderer_server_command(
    message: QConnectMessage,
) -> Result<Option<RendererServerCommand>, ProtocolError> {
    let message_type = resolve_renderer_message_type(&message);
    let Some(message_type) = message_type else {
        return Ok(None);
    };

    let command = match message_type {
        code if code == QConnectMessageType::MessageTypeSrvrRndrSetState as i32 => {
            let Some(payload) = message.srvr_rndr_set_state else {
                return Ok(None);
            };
            map_srvr_rndr_set_state(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrRndrSetVolume as i32 => {
            let Some(payload) = message.srvr_rndr_set_volume else {
                return Ok(None);
            };
            map_srvr_rndr_set_volume(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrRndrSetActive as i32 => {
            let Some(payload) = message.srvr_rndr_set_active else {
                return Ok(None);
            };
            map_srvr_rndr_set_active(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrRndrSetMaxAudioQuality as i32 => {
            let Some(payload) = message.srvr_rndr_set_max_audio_quality else {
                return Ok(None);
            };
            map_srvr_rndr_set_max_audio_quality(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrRndrSetLoopMode as i32 => {
            let Some(payload) = message.srvr_rndr_set_loop_mode else {
                return Ok(None);
            };
            map_srvr_rndr_set_loop_mode(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrRndrSetShuffleMode as i32 => {
            let Some(payload) = message.srvr_rndr_set_shuffle_mode else {
                return Ok(None);
            };
            map_srvr_rndr_set_shuffle_mode(payload)?
        }
        code if code == QConnectMessageType::MessageTypeSrvrRndrMuteVolume as i32 => {
            let Some(payload) = message.srvr_rndr_mute_volume else {
                return Ok(None);
            };
            map_srvr_rndr_mute_volume(payload)?
        }
        _ => return Ok(None),
    };

    Ok(Some(command))
}

fn resolve_renderer_message_type(message: &QConnectMessage) -> Option<i32> {
    message.message_type.or_else(|| {
        if message.srvr_rndr_set_state.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrRndrSetState as i32);
        }
        if message.srvr_rndr_set_volume.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrRndrSetVolume as i32);
        }
        if message.srvr_rndr_set_active.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrRndrSetActive as i32);
        }
        if message.srvr_rndr_set_max_audio_quality.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrRndrSetMaxAudioQuality as i32);
        }
        if message.srvr_rndr_set_loop_mode.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrRndrSetLoopMode as i32);
        }
        if message.srvr_rndr_set_shuffle_mode.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrRndrSetShuffleMode as i32);
        }
        if message.srvr_rndr_mute_volume.is_some() {
            return Some(QConnectMessageType::MessageTypeSrvrRndrMuteVolume as i32);
        }
        None
    })
}

fn map_srvr_rndr_set_state(
    payload: RendererSetStateMessage,
) -> Result<RendererServerCommand, ProtocolError> {
    let current_track = payload
        .current_track
        .map(queue_track_with_context_to_json)
        .transpose()?;
    let next_track = payload
        .next_track
        .map(queue_track_with_context_to_json)
        .transpose()?;
    let queue_version = queue_version_opt(payload.queue_version)?;

    Ok(RendererServerCommand {
        command_type: RendererCommandType::SrvrRndrSetState,
        payload: json!({
            "playing_state": payload.playing_state,
            "current_position": optional_i32_to_u64(payload.current_position)?,
            "queue_version": queue_version,
            "current_track": current_track,
            "next_track": next_track
        }),
    })
}

fn map_srvr_rndr_set_volume(
    payload: RendererSetVolumeMessage,
) -> Result<RendererServerCommand, ProtocolError> {
    Ok(RendererServerCommand {
        command_type: RendererCommandType::SrvrRndrSetVolume,
        payload: json!({
            "volume": payload.volume,
            "volume_delta": payload.volume_delta
        }),
    })
}

fn map_srvr_rndr_set_active(
    payload: RendererSetActiveMessage,
) -> Result<RendererServerCommand, ProtocolError> {
    Ok(RendererServerCommand {
        command_type: RendererCommandType::SrvrRndrSetActive,
        payload: json!({
            "active": payload.active.unwrap_or(false)
        }),
    })
}

fn map_srvr_rndr_set_max_audio_quality(
    payload: RendererSetMaxAudioQualityMessage,
) -> Result<RendererServerCommand, ProtocolError> {
    Ok(RendererServerCommand {
        command_type: RendererCommandType::SrvrRndrSetMaxAudioQuality,
        payload: json!({
            "max_audio_quality": payload.max_audio_quality
        }),
    })
}

fn map_srvr_rndr_set_loop_mode(
    payload: RendererSetLoopModeMessage,
) -> Result<RendererServerCommand, ProtocolError> {
    Ok(RendererServerCommand {
        command_type: RendererCommandType::SrvrRndrSetLoopMode,
        payload: json!({
            "loop_mode": payload.loop_mode
        }),
    })
}

fn map_srvr_rndr_set_shuffle_mode(
    payload: RendererSetShuffleModeMessage,
) -> Result<RendererServerCommand, ProtocolError> {
    Ok(RendererServerCommand {
        command_type: RendererCommandType::SrvrRndrSetShuffleMode,
        payload: json!({
            "shuffle_mode": payload.shuffle_mode.unwrap_or(false)
        }),
    })
}

fn map_srvr_rndr_mute_volume(
    payload: RendererMuteVolumeMessage,
) -> Result<RendererServerCommand, ProtocolError> {
    Ok(RendererServerCommand {
        command_type: RendererCommandType::SrvrRndrMuteVolume,
        payload: json!({
            "value": payload.value.unwrap_or(false)
        }),
    })
}

fn map_queue_error(payload: QueueErrorMessage) -> Result<QueueServerEvent, ProtocolError> {
    let error_code = payload
        .error
        .as_ref()
        .and_then(|err| err.code.clone())
        .unwrap_or_else(|| "remote_error".to_string());
    let error_message = payload
        .error
        .as_ref()
        .and_then(|err| err.message.clone())
        .unwrap_or_else(|| "queue_error_message".to_string());

    log::warn!(
        "[QConnect/Decode] Queue error from server: code={error_code} message={error_message}"
    );

    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlQueueErrorMessage,
        action_uuid: uuid_bytes_to_string_opt(payload.action_uuid, "queue_error.action_uuid")?,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({
            "error_code": error_code,
            "error_message": error_message
        }),
    })
}

fn map_queue_cleared(payload: QueueClearedMessage) -> Result<QueueServerEvent, ProtocolError> {
    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlQueueCleared,
        action_uuid: uuid_bytes_to_string_opt(payload.action_uuid, "queue_cleared.action_uuid")?,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({}),
    })
}

fn map_queue_state(payload: QueueStateMessage) -> Result<QueueServerEvent, ProtocolError> {
    let tracks = payload
        .tracks
        .into_iter()
        .map(queue_track_with_context_to_json)
        .collect::<Result<Vec<_>, _>>()?;
    let autoplay_tracks = payload
        .autoplay_tracks
        .into_iter()
        .map(queue_track_with_context_to_json)
        .collect::<Result<Vec<_>, _>>()?;

    let shuffled_indexes = payload
        .shuffled_track_indexes
        .into_iter()
        .map(i32_to_u64)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlQueueState,
        action_uuid: uuid_bytes_to_string_opt(payload.action_uuid, "queue_state.action_uuid")?,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({
            "tracks": tracks,
            "shuffle_mode": payload.shuffle_mode.unwrap_or(false),
            "shuffled_track_indexes": shuffled_indexes,
            "autoplay_mode": payload.autoplay_mode.unwrap_or(false),
            "autoplay_loading": payload.autoplay_loading.unwrap_or(false),
            "autoplay_tracks": autoplay_tracks,
            // Field #100: previously decoded-then-dropped. Surfaced for the
            // queue_hash divergence seam (algorithm is a BLOCKING unknown).
            "server_queue_hash": payload.queue_hash
        }),
    })
}

fn map_tracks_loaded(payload: QueueTracksLoadedMessage) -> Result<QueueServerEvent, ProtocolError> {
    let context_uuid =
        uuid_bytes_to_string_opt(payload.context_uuid, "tracks_loaded.context_uuid")?;
    let tracks = payload
        .tracks
        .into_iter()
        .map(|track| queue_track_to_json(track, context_uuid.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlQueueTracksLoaded,
        action_uuid: uuid_bytes_to_string_opt(payload.action_uuid, "tracks_loaded.action_uuid")?,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({
            "tracks": tracks,
            "queue_position": optional_i32_to_u64(payload.queue_position)?,
            "shuffle_seed": payload.shuffle_seed.map(|v| v as u64),
            "shuffle_pivot_queue_item_id": optional_i32_to_u64(payload.shuffle_pivot_queue_item_id)?,
            "shuffle_mode": payload.shuffle_mode.unwrap_or(false),
            "autoplay_reset": payload.autoplay_reset.unwrap_or(false),
            "autoplay_loading": payload.autoplay_loading.unwrap_or(false)
        }),
    })
}

fn map_tracks_inserted(
    payload: QueueTracksInsertedMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    let context_uuid =
        uuid_bytes_to_string_opt(payload.context_uuid, "tracks_inserted.context_uuid")?;
    let tracks = payload
        .tracks
        .into_iter()
        .map(|track| queue_track_to_json(track, context_uuid.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlQueueTracksInserted,
        action_uuid: uuid_bytes_to_string_opt(payload.action_uuid, "tracks_inserted.action_uuid")?,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({
            "tracks": tracks,
            "insert_after": optional_i32_to_u64(payload.insert_after)?,
            "shuffle_seed": payload.shuffle_seed.map(|v| v as u64),
            "autoplay_reset": payload.autoplay_reset.unwrap_or(false),
            "autoplay_loading": payload.autoplay_loading.unwrap_or(false)
        }),
    })
}

fn map_tracks_added(payload: QueueTracksAddedMessage) -> Result<QueueServerEvent, ProtocolError> {
    let context_uuid = uuid_bytes_to_string_opt(payload.context_uuid, "tracks_added.context_uuid")?;
    let tracks = payload
        .tracks
        .into_iter()
        .map(|track| queue_track_to_json(track, context_uuid.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlQueueTracksAdded,
        action_uuid: uuid_bytes_to_string_opt(payload.action_uuid, "tracks_added.action_uuid")?,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({
            "tracks": tracks,
            "shuffle_seed": payload.shuffle_seed.map(|v| v as u64),
            "autoplay_reset": payload.autoplay_reset.unwrap_or(false),
            "autoplay_loading": payload.autoplay_loading.unwrap_or(false)
        }),
    })
}

fn map_tracks_removed(
    payload: QueueTracksRemovedMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    let queue_item_ids = payload
        .queue_item_ids
        .into_iter()
        .map(i32_to_u64)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlQueueTracksRemoved,
        action_uuid: uuid_bytes_to_string_opt(payload.action_uuid, "tracks_removed.action_uuid")?,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({
            "queue_item_ids": queue_item_ids,
            "autoplay_reset": payload.autoplay_reset.unwrap_or(false),
            "autoplay_loading": payload.autoplay_loading.unwrap_or(false)
        }),
    })
}

fn map_tracks_reordered(
    payload: QueueTracksReorderedMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    let queue_item_ids = payload
        .queue_item_ids
        .into_iter()
        .map(i32_to_u64)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlQueueTracksReordered,
        action_uuid: uuid_bytes_to_string_opt(payload.action_uuid, "tracks_reordered.action_uuid")?,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({
            "queue_item_ids": queue_item_ids,
            "insert_after": optional_i32_to_u64(payload.insert_after)?,
            "autoplay_reset": payload.autoplay_reset.unwrap_or(false),
            "autoplay_loading": payload.autoplay_loading.unwrap_or(false)
        }),
    })
}

fn map_shuffle_mode_set(payload: ShuffleModeSetMessage) -> Result<QueueServerEvent, ProtocolError> {
    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlShuffleModeSet,
        action_uuid: uuid_bytes_to_string_opt(payload.action_uuid, "shuffle_mode_set.action_uuid")?,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({
            "shuffle_mode": payload.shuffle_mode.unwrap_or(false),
            "shuffle_seed": payload.shuffle_seed.map(|v| v as u64),
            "shuffle_pivot_queue_item_id": optional_i32_to_u64(payload.shuffle_pivot_queue_item_id)?,
            "autoplay_reset": payload.autoplay_reset.unwrap_or(false),
            "autoplay_loading": payload.autoplay_loading.unwrap_or(false)
        }),
    })
}

fn map_autoplay_mode_set(
    payload: AutoplayModeSetMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlAutoplayModeSet,
        action_uuid: uuid_bytes_to_string_opt(
            payload.action_uuid,
            "autoplay_mode_set.action_uuid",
        )?,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({
            "autoplay_mode": payload.autoplay_mode.unwrap_or(false),
            "autoplay_reset": payload.autoplay_reset.unwrap_or(false),
            "autoplay_loading": payload.autoplay_loading.unwrap_or(false)
        }),
    })
}

fn map_autoplay_tracks_loaded(
    payload: AutoplayTracksLoadedMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    let context_uuid =
        uuid_bytes_to_string_opt(payload.context_uuid, "autoplay_tracks_loaded.context_uuid")?;
    let tracks = payload
        .tracks
        .into_iter()
        .map(|track| queue_track_to_json(track, context_uuid.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlAutoplayTracksLoaded,
        action_uuid: uuid_bytes_to_string_opt(
            payload.action_uuid,
            "autoplay_tracks_loaded.action_uuid",
        )?,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({
            "tracks": tracks
        }),
    })
}

fn map_autoplay_tracks_removed(
    payload: AutoplayTracksRemovedMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    let queue_item_ids = payload
        .queue_item_ids
        .into_iter()
        .map(i32_to_u64)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlAutoplayTracksRemoved,
        action_uuid: uuid_bytes_to_string_opt(
            payload.action_uuid,
            "autoplay_tracks_removed.action_uuid",
        )?,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({
            "queue_item_ids": queue_item_ids
        }),
    })
}

fn map_tracks_added_from_autoplay(
    payload: QueueTracksAddedFromAutoplayMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    let queue_item_ids = payload
        .queue_item_ids
        .into_iter()
        .map(i32_to_u64)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlQueueTracksAddedFromAutoplay,
        action_uuid: None,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({
            "queue_item_ids": queue_item_ids
        }),
    })
}

// --- Session management event mappers ---

fn map_session_state(payload: CtrlSessionStateMessage) -> Result<QueueServerEvent, ProtocolError> {
    let session_uuid =
        uuid_bytes_to_string_opt(payload.session_uuid, "session_state.session_uuid")?;
    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlSessionState,
        action_uuid: None,
        queue_version: queue_version_opt(payload.queue_version)?,
        payload: json!({
            "session_uuid": session_uuid,
            "active_renderer_id": payload.active_renderer_id,
            "playing_state": payload.playing_state,
            "loop_mode": payload.loop_mode
        }),
    })
}

fn map_ctrl_renderer_state_updated(
    payload: CtrlRendererStateUpdatedMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    let player_state = payload.player_state.map(|ps| {
        json!({
            "playing_state": ps.playing_state.unwrap_or(0),
            "buffer_state": ps.buffer_state.unwrap_or(0),
            "current_position": ps.current_position.as_ref().map(|pos| pos.value.unwrap_or(0)),
            "duration": ps.duration.unwrap_or(0),
            // Official proto3 RendererPlayerState defaults an omitted scalar
            // cursor to 0. Only an absent player_state is absent; -1 is the
            // explicit no-track sentinel. Treating zero as unknown strands
            // next/previous on the first item after a peer takeover.
            "current_queue_item_id": ps.current_queue_item_id.unwrap_or(0)
        })
    });

    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlRendererStateUpdated,
        action_uuid: None,
        queue_version: None,
        payload: json!({
            "renderer_id": payload.renderer_id,
            "status": payload.status,
            "player_state": player_state
        }),
    })
}

fn map_add_renderer(payload: CtrlAddRendererMessage) -> Result<QueueServerEvent, ProtocolError> {
    let device_info = payload.device_info.map(|di| {
        json!({
            "friendly_name": di.friendly_name,
            "brand": di.brand,
            "model": di.model,
            "device_type": di.device_type,
            // Controllers must retain the renderer's explicit restrictions.
            "capabilities": di.capabilities.map(|capabilities| json!({
                "min_audio_quality": capabilities.min_audio_quality,
                "max_audio_quality": capabilities.max_audio_quality,
                "volume_remote_control": capabilities.volume_remote_control,
            }))
        })
    });

    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlAddRenderer,
        action_uuid: None,
        queue_version: None,
        payload: json!({
            "renderer_id": payload.renderer_id,
            "device_info": device_info
        }),
    })
}

fn map_update_renderer(
    payload: CtrlUpdateRendererMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    let device_info = payload.device_info.map(|di| {
        json!({
            "friendly_name": di.friendly_name,
            "brand": di.brand,
            "model": di.model,
            "device_type": di.device_type,
            // Controllers must retain the renderer's explicit restrictions.
            "capabilities": di.capabilities.map(|capabilities| json!({
                "min_audio_quality": capabilities.min_audio_quality,
                "max_audio_quality": capabilities.max_audio_quality,
                "volume_remote_control": capabilities.volume_remote_control,
            }))
        })
    });

    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlUpdateRenderer,
        action_uuid: None,
        queue_version: None,
        payload: json!({
            "renderer_id": payload.renderer_id,
            "device_info": device_info
        }),
    })
}

fn map_remove_renderer(
    payload: CtrlRemoveRendererMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlRemoveRenderer,
        action_uuid: None,
        queue_version: None,
        payload: json!({
            "renderer_id": payload.renderer_id
        }),
    })
}

fn map_active_renderer_changed(
    payload: CtrlActiveRendererChangedMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlActiveRendererChanged,
        action_uuid: None,
        queue_version: None,
        payload: json!({
            "active_renderer_id": payload.active_renderer_id
        }),
    })
}

fn map_ctrl_volume_changed(
    payload: CtrlVolumeChangedMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlVolumeChanged,
        action_uuid: None,
        queue_version: None,
        payload: json!({
            "renderer_id": payload.renderer_id,
            "volume": payload.volume.unwrap_or(0)
        }),
    })
}

fn map_ctrl_loop_mode_set(
    payload: CtrlLoopModeSetMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlLoopModeSet,
        action_uuid: None,
        queue_version: None,
        payload: json!({
            "loop_mode": payload.loop_mode
        }),
    })
}

fn map_ctrl_volume_muted(
    payload: CtrlVolumeMutedMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlVolumeMuted,
        action_uuid: None,
        queue_version: None,
        payload: json!({
            "renderer_id": payload.renderer_id,
            "value": payload.value.unwrap_or(false)
        }),
    })
}

fn map_ctrl_max_audio_quality_changed(
    payload: CtrlMaxAudioQualityChangedMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlMaxAudioQualityChanged,
        action_uuid: None,
        queue_version: None,
        payload: json!({
            "renderer_id": payload.renderer_id,
            "max_audio_quality": payload.max_audio_quality,
            "network_type": payload.network_type
        }),
    })
}

fn map_ctrl_file_audio_quality_changed(
    payload: CtrlFileAudioQualityChangedMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlFileAudioQualityChanged,
        action_uuid: None,
        queue_version: None,
        payload: json!({
            "renderer_id": payload.renderer_id,
            "sampling_rate": payload.sampling_rate,
            "bit_depth": payload.bit_depth,
            "nb_channels": payload.nb_channels,
            "audio_quality": payload.audio_quality
        }),
    })
}

fn map_ctrl_device_audio_quality_changed(
    payload: CtrlDeviceAudioQualityChangedMessage,
) -> Result<QueueServerEvent, ProtocolError> {
    Ok(QueueServerEvent {
        event_type: QueueEventType::SrvrCtrlDeviceAudioQualityChanged,
        action_uuid: None,
        queue_version: None,
        payload: json!({
            "renderer_id": payload.renderer_id,
            "sampling_rate": payload.sampling_rate,
            "bit_depth": payload.bit_depth,
            "nb_channels": payload.nb_channels
        }),
    })
}

fn queue_track_to_json(
    track: QueueTrack,
    context_uuid: Option<&str>,
) -> Result<Value, ProtocolError> {
    let track_id = track.track_id.map(|v| v as u64).ok_or_else(|| {
        ProtocolError::InvalidPayload("missing required field 'queue_track.track_id'".to_string())
    })?;
    let queue_item_id =
        optional_i32_to_u64_named(track.queue_item_id, "queue_track.queue_item_id")?.unwrap_or(0);

    Ok(json!({
        "track_context_uuid": context_uuid.unwrap_or_default(),
        "track_id": track_id,
        "queue_item_id": queue_item_id
    }))
}

fn queue_track_with_context_to_json(track: QueueTrackWithContext) -> Result<Value, ProtocolError> {
    let track_id = track.track_id.map(|v| v as u64).ok_or_else(|| {
        ProtocolError::InvalidPayload(
            "missing required field 'queue_track_with_context.track_id'".to_string(),
        )
    })?;
    let queue_item_id = optional_i32_to_u64_named(
        track.queue_item_id,
        "queue_track_with_context.queue_item_id",
    )?
    .unwrap_or(0);
    let context_uuid =
        uuid_bytes_to_string_opt(track.context_uuid, "queue_track_with_context.context_uuid")?
            .unwrap_or_default();

    Ok(json!({
        "track_context_uuid": context_uuid,
        "track_id": track_id,
        "queue_item_id": queue_item_id
    }))
}

fn queue_version_opt(
    value: Option<QueueVersionRef>,
) -> Result<Option<QueueVersion>, ProtocolError> {
    let Some(version) = value else {
        return Ok(None);
    };

    // Qobuz may send QueueVersionRef with missing major/minor — default to 0
    let major = version
        .major
        .map(|v| i32_to_u64(v))
        .transpose()?
        .unwrap_or(0);
    let minor = version
        .minor
        .map(|v| i32_to_u64(v))
        .transpose()?
        .unwrap_or(0);
    Ok(Some(QueueVersion::new(major, minor)))
}

fn optional_i32_to_u64_named(
    value: Option<i32>,
    field_name: &str,
) -> Result<Option<u64>, ProtocolError> {
    value
        .map(|raw| {
            if raw < 0 {
                return Err(ProtocolError::InvalidPayload(format!(
                    "negative value where unsigned expected in '{field_name}': {raw}"
                )));
            }
            Ok(raw as u64)
        })
        .transpose()
}

fn optional_i32_to_u64(value: Option<i32>) -> Result<Option<u64>, ProtocolError> {
    value.map(i32_to_u64).transpose()
}

fn i32_to_u64(value: i32) -> Result<u64, ProtocolError> {
    if value < 0 {
        return Err(ProtocolError::InvalidPayload(format!(
            "negative value where unsigned expected: {value}"
        )));
    }
    Ok(value as u64)
}

fn uuid_bytes_to_string_opt(
    value: Option<Vec<u8>>,
    field_name: &str,
) -> Result<Option<String>, ProtocolError> {
    let Some(bytes) = value else {
        return Ok(None);
    };
    let uuid = Uuid::from_slice(&bytes).map_err(|err| {
        ProtocolError::InvalidUuid(format!("{field_name} invalid UUID bytes: {err}"))
    })?;
    Ok(Some(uuid.to_string()))
}

#[cfg(test)]
mod tests {
    use prost::Message;

    use crate::queue_command_proto::{
        PlaybackErrorMessage, QConnectMessage, QConnectMessageType, QConnectMessages, QueueTrack,
        QueueTrackWithContext, QueueTracksAddedMessage, QueueTracksLoadedMessage, QueueVersionRef,
        RendererMuteVolumeMessage, RendererSetStateMessage,
    };
    use crate::ErrorType;

    use super::{
        decode_playback_error, decode_queue_server_events, decode_renderer_server_commands,
    };

    #[test]
    fn controller_renderer_capabilities_survive_add_and_update_frames() {
        use crate::queue_command_proto::{
            CtrlAddRendererMessage, CtrlUpdateRendererMessage, DeviceCapabilitiesMessage,
            DeviceInfoMessage,
        };
        for volume in [None, Some(0), Some(1), Some(2)] {
            for update in [false, true] {
                let info = DeviceInfoMessage {
                    friendly_name: Some("renderer".into()),
                    capabilities: Some(DeviceCapabilitiesMessage {
                        min_audio_quality: Some(1),
                        max_audio_quality: Some(4),
                        volume_remote_control: volume,
                    }),
                    ..Default::default()
                };
                let mut message = QConnectMessage::default();
                if update {
                    message.srvr_ctrl_update_renderer = Some(CtrlUpdateRendererMessage {
                        renderer_id: Some(2),
                        device_info: Some(info),
                    });
                } else {
                    message.srvr_ctrl_add_renderer = Some(CtrlAddRendererMessage {
                        renderer_id: Some(2),
                        device_info: Some(info),
                    });
                }
                let batch = QConnectMessages {
                    messages: vec![message],
                    ..Default::default()
                };
                let events = decode_queue_server_events(&batch.encode_to_vec()).unwrap();
                assert_eq!(
                    events[0].payload["device_info"]["capabilities"],
                    serde_json::json!({
                        "min_audio_quality": 1, "max_audio_quality": 4, "volume_remote_control": volume,
                    }),
                    "update={update}, volume={volume:?}"
                );
            }
        }
    }

    #[test]
    fn controller_state_proto3_zero_cursor_and_position_are_not_missing() {
        // Official proto3 encoder omits scalar zero, including queue item 0.
        // Type 82, renderer 1, PLAY, present-but-empty position, no cursor tag.
        let raw = [
            0x08, 0x52, 0x92, 0x05, 0x08, 0x08, 0x01, 0x1a, 0x04, 0x08, 0x02, 0x1a, 0x00,
        ];
        let batch = QConnectMessages {
            messages: vec![QConnectMessage::decode(raw.as_slice()).unwrap()],
            ..Default::default()
        };
        let events = decode_queue_server_events(&batch.encode_to_vec()).unwrap();
        let state = &events[0].payload["player_state"];
        assert_eq!(state["current_queue_item_id"], 0);
        assert_eq!(state["current_position"], 0);
        assert_eq!(state["playing_state"], 2);
    }

    #[test]
    fn controller_state_absent_player_or_position_stays_absent() {
        use crate::queue_command_proto::{
            CtrlRendererPlayerState, CtrlRendererStateUpdatedMessage,
        };
        for player_state in [
            None,
            Some(CtrlRendererPlayerState {
                current_queue_item_id: Some(-1),
                ..Default::default()
            }),
        ] {
            let has_player = player_state.is_some();
            let batch = QConnectMessages {
                messages: vec![QConnectMessage {
                    srvr_ctrl_renderer_state_updated: Some(CtrlRendererStateUpdatedMessage {
                        renderer_id: Some(1),
                        player_state,
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let events = decode_queue_server_events(&batch.encode_to_vec()).unwrap();
            let state = &events[0].payload["player_state"];
            if has_player {
                assert_eq!(state["current_queue_item_id"], -1);
                assert!(state["current_position"].is_null());
            } else {
                assert!(state.is_null());
            }
        }
    }

    #[test]
    fn controller_volume_zero_and_unmute_decode_official_scalar_defaults() {
        use crate::queue_command_proto::{CtrlVolumeChangedMessage, CtrlVolumeMutedMessage};
        let batch = QConnectMessages {
            messages: vec![
                QConnectMessage {
                    srvr_ctrl_volume_changed: Some(CtrlVolumeChangedMessage {
                        renderer_id: Some(2),
                        volume: None,
                    }),
                    ..Default::default()
                },
                QConnectMessage {
                    srvr_ctrl_volume_muted: Some(CtrlVolumeMutedMessage {
                        renderer_id: Some(2),
                        value: None,
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let events = decode_queue_server_events(&batch.encode_to_vec()).unwrap();
        assert_eq!(events[0].payload["volume"], 0);
        assert_eq!(events[1].payload["value"], false);
    }

    #[test]
    fn decodes_playback_error_track_not_streamable() {
        // playback_error rides at tag 3 of a QConnectMessage inside the batch
        // messages[], not on the QConnectMessages wrapper (verified from the
        // transport bytes: decoder receives the batch).
        let message = QConnectMessage {
            // message_type = PLAYBACK_ERROR(2) per Common.MessageType.
            message_type: Some(2),
            playback_error: Some(PlaybackErrorMessage {
                queue_version: Some(QueueVersionRef {
                    major: Some(2),
                    minor: Some(0),
                }),
                queue_item_id: Some(55),
                error_type: Some(2), // TRACK_NOT_STREAMABLE
            }),
            ..Default::default()
        };
        let batch = QConnectMessages {
            messages_time: None,
            messages_id: None,
            messages: vec![message],
        };
        let bytes = batch.encode_to_vec();
        let parsed = decode_playback_error(&bytes).expect("playback error decoded");
        assert_eq!(parsed.queue_item_id, 55);
        assert_eq!(parsed.error_type, ErrorType::TrackNotStreamable);
    }

    #[test]
    fn decodes_tracks_added_server_event_batch() {
        let message = QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeSrvrCtrlQueueTracksAdded as i32),
            srvr_ctrl_queue_tracks_added: Some(QueueTracksAddedMessage {
                queue_version: Some(QueueVersionRef {
                    major: Some(3),
                    minor: Some(4),
                }),
                action_uuid: Some(
                    uuid::Uuid::parse_str("85fa0dd6-7bd6-4b3c-8f43-b8ee22e65d5e")
                        .expect("uuid")
                        .as_bytes()
                        .to_vec(),
                ),
                tracks: vec![QueueTrack {
                    queue_item_id: Some(44),
                    track_id: Some(555),
                }],
                shuffle_seed: Some(99),
                context_uuid: Some(
                    uuid::Uuid::parse_str("0f892e1a-a2f4-4d18-82c6-31e8daf2ea0f")
                        .expect("context uuid")
                        .as_bytes()
                        .to_vec(),
                ),
                autoplay_reset: Some(false),
                autoplay_loading: Some(false),
                queue_hash: None,
            }),
            ..Default::default()
        };

        let batch = QConnectMessages {
            messages_time: Some(1),
            messages_id: Some(1),
            messages: vec![message],
        };
        let encoded = batch.encode_to_vec();

        let events = decode_queue_server_events(&encoded).expect("decode events");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].message_type(),
            "MESSAGE_TYPE_SRVR_CTRL_QUEUE_TRACKS_ADDED"
        );
    }

    #[test]
    fn decodes_srvr_rndr_set_state_command_batch() {
        let message = QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeSrvrRndrSetState as i32),
            srvr_rndr_set_state: Some(RendererSetStateMessage {
                playing_state: Some(2),
                current_position: Some(42_000),
                queue_version: Some(QueueVersionRef {
                    major: Some(7),
                    minor: Some(8),
                }),
                current_track: Some(QueueTrackWithContext {
                    queue_item_id: Some(1001),
                    track_id: Some(555_666),
                    context_uuid: Some(
                        uuid::Uuid::parse_str("95f28997-6c88-47cc-a535-9f8e5b9c5fe1")
                            .expect("context uuid")
                            .as_bytes()
                            .to_vec(),
                    ),
                }),
                next_track: None,
            }),
            ..Default::default()
        };

        let batch = QConnectMessages {
            messages_time: Some(1),
            messages_id: Some(2),
            messages: vec![message],
        };
        let encoded = batch.encode_to_vec();

        let commands = decode_renderer_server_commands(&encoded).expect("decode renderer commands");
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].message_type(),
            "MESSAGE_TYPE_SRVR_RNDR_SET_STATE"
        );
        assert_eq!(commands[0].payload["playing_state"], 2);
        assert_eq!(commands[0].payload["current_position"], 42_000);
    }

    #[test]
    fn decodes_missing_queue_item_id_as_zero_in_queue_events() {
        let message = QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeSrvrCtrlQueueTracksLoaded as i32),
            srvr_ctrl_queue_tracks_loaded: Some(QueueTracksLoadedMessage {
                queue_version: Some(QueueVersionRef {
                    major: Some(7),
                    minor: Some(1),
                }),
                action_uuid: Some(
                    uuid::Uuid::parse_str("f2b1f0a4-3d0a-4a67-b234-4d1df4d58c8f")
                        .expect("uuid")
                        .as_bytes()
                        .to_vec(),
                ),
                tracks: vec![QueueTrack {
                    queue_item_id: None,
                    track_id: Some(126_886_862),
                }],
                queue_position: Some(0),
                shuffle_mode: Some(false),
                shuffle_seed: None,
                shuffle_pivot_queue_item_id: None,
                context_uuid: Some(
                    uuid::Uuid::parse_str("4c321d71-aef2-4c98-8cd3-8d2ad4bfe0f4")
                        .expect("context uuid")
                        .as_bytes()
                        .to_vec(),
                ),
                autoplay_reset: Some(true),
                autoplay_loading: Some(false),
                queue_hash: None,
            }),
            ..Default::default()
        };

        let batch = QConnectMessages {
            messages_time: Some(1),
            messages_id: Some(4),
            messages: vec![message],
        };
        let encoded = batch.encode_to_vec();

        let events = decode_queue_server_events(&encoded).expect("decode queue events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["tracks"][0]["queue_item_id"], 0);
        assert_eq!(events[0].payload["tracks"][0]["track_id"], 126_886_862);
    }

    #[test]
    fn decodes_missing_queue_item_id_as_zero_in_renderer_state() {
        let message = QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeSrvrRndrSetState as i32),
            srvr_rndr_set_state: Some(RendererSetStateMessage {
                playing_state: Some(2),
                current_position: Some(8_000),
                queue_version: Some(QueueVersionRef {
                    major: Some(7),
                    minor: Some(1),
                }),
                current_track: Some(QueueTrackWithContext {
                    queue_item_id: None,
                    track_id: Some(126_886_862),
                    context_uuid: Some(
                        uuid::Uuid::parse_str("4c321d71-aef2-4c98-8cd3-8d2ad4bfe0f4")
                            .expect("context uuid")
                            .as_bytes()
                            .to_vec(),
                    ),
                }),
                next_track: Some(QueueTrackWithContext {
                    queue_item_id: Some(1),
                    track_id: Some(25_584_418),
                    context_uuid: Some(
                        uuid::Uuid::parse_str("4c321d71-aef2-4c98-8cd3-8d2ad4bfe0f4")
                            .expect("context uuid")
                            .as_bytes()
                            .to_vec(),
                    ),
                }),
            }),
            ..Default::default()
        };

        let batch = QConnectMessages {
            messages_time: Some(1),
            messages_id: Some(5),
            messages: vec![message],
        };
        let encoded = batch.encode_to_vec();

        let commands = decode_renderer_server_commands(&encoded).expect("decode renderer commands");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].payload["current_track"]["queue_item_id"], 0);
        assert_eq!(
            commands[0].payload["current_track"]["track_id"],
            126_886_862
        );
        assert_eq!(commands[0].payload["next_track"]["queue_item_id"], 1);
    }

    #[test]
    fn decodes_srvr_rndr_mute_volume_command_batch() {
        let message = QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeSrvrRndrMuteVolume as i32),
            srvr_rndr_mute_volume: Some(RendererMuteVolumeMessage { value: Some(true) }),
            ..Default::default()
        };

        let batch = QConnectMessages {
            messages_time: Some(1),
            messages_id: Some(3),
            messages: vec![message],
        };
        let encoded = batch.encode_to_vec();

        let commands = decode_renderer_server_commands(&encoded).expect("decode renderer commands");
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].message_type(),
            "MESSAGE_TYPE_SRVR_RNDR_MUTE_VOLUME"
        );
        assert_eq!(commands[0].payload["value"], true);
    }

    #[test]
    fn decodes_official_renderer_state_updated_fixed64_timestamp_frame() {
        let raw_message = vec![
            0x08, 0x17, 0xba, 0x01, 0x21, 0x0a, 0x1f, 0x08, 0x02, 0x10, 0x02, 0x1a, 0x0d, 0x09,
            0xe1, 0x08, 0x7e, 0xe5, 0x9c, 0x01, 0x00, 0x00, 0x10, 0xa1, 0xec, 0x01, 0x20, 0xa8,
            0x9e, 0x14, 0x2a, 0x04, 0x08, 0x09, 0x10, 0x02, 0x38, 0x0c,
        ];

        let message = QConnectMessage::decode(raw_message.as_slice()).expect("decode message");
        let state = message
            .rndr_srvr_state_updated
            .as_ref()
            .and_then(|payload| payload.state.as_ref())
            .expect("renderer state");
        let position = state.current_position.as_ref().expect("playback position");

        assert_eq!(
            message.message_type,
            Some(QConnectMessageType::MessageTypeRndrSrvrStateUpdated as i32)
        );
        assert_eq!(position.timestamp, Some(1_773_376_768_225));
        assert_eq!(position.value, Some(30_241));
        assert_eq!(state.current_queue_item_id, None);
        assert_eq!(state.next_queue_item_id, Some(12));
    }
}
