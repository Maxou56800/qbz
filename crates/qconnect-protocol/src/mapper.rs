use std::{
    sync::atomic::{AtomicI32, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use prost::Message;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    build_outbound_envelope,
    queue_command_proto::{
        AskForQueueStateMessage, AskForRendererStateMessage, AutoplayLoadTracksMessage,
        AutoplayRemoveTracksMessage, ClearQueueMessage, DeviceCapabilitiesMessage,
        DeviceInfoMessage, RendererDeviceInfoUpdatedMessage, JoinSessionMessage, MuteVolumeMessage, PlaybackPositionMessage,
        QConnectMessage, QConnectMessageType, QConnectMessages, QueueAddTracksMessage,
        QueueInsertTracksMessage, QueueLoadTracksMessage, QueueRemoveTracksMessage,
        QueueReorderTracksMessage, QueueVersionRef, RendererDeviceAudioQualityChangedMessage,
        RendererFileAudioQualityChangedMessage,
        RendererMaxAudioQualityChangedMessage, RendererStateMessage, RendererStateUpdatedMessage,
        RendererVolumeChangedMessage, RendererVolumeMutedMessage, SetActiveRendererMessage,
        SetAutoplayModeMessage, SetLoopModeMessage, SetMaxAudioQualityMessage,
        SetPlayerStateMessage, SetPlayerStateQueueItemMessage, SetQueueStateMessage,
        SetQueueTrackWithContext, SetShuffleModeMessage, SetVolumeMessage,
    },
    OutboundEnvelope, ProtocolError, QueueCommand, QueueCommandType, RendererReport,
    RendererReportType,
};

static BATCH_SEQ: AtomicI32 = AtomicI32::new(1);

pub fn build_qconnect_outbound_envelope(
    command: QueueCommand,
) -> Result<OutboundEnvelope, ProtocolError> {
    let payload_bytes = encode_queue_command_batch(&command)?;
    let mut envelope = build_outbound_envelope(command);
    envelope.payload_bytes = Some(payload_bytes);
    Ok(envelope)
}

pub fn encode_queue_command_batch(command: &QueueCommand) -> Result<Vec<u8>, ProtocolError> {
    let message = map_queue_command(command)?;
    let batch = QConnectMessages {
        messages_time: Some(now_ms()),
        messages_id: Some(next_batch_seq()),
        messages: vec![message],
    };
    Ok(batch.encode_to_vec())
}

pub fn build_qconnect_renderer_outbound_envelope(
    report: RendererReport,
) -> Result<OutboundEnvelope, ProtocolError> {
    let payload_bytes = encode_renderer_report_batch(&report)?;
    let message_type = report.message_type().to_string();
    let mut envelope = build_outbound_envelope(QueueCommand::new(
        QueueCommandType::CtrlSrvrAskForQueueState,
        report.action_uuid,
        report.queue_version_ref,
        report.payload,
    ));
    envelope.message_type = message_type;
    envelope.payload_bytes = Some(payload_bytes);
    Ok(envelope)
}

pub fn encode_renderer_report_batch(report: &RendererReport) -> Result<Vec<u8>, ProtocolError> {
    let message = map_renderer_report(report)?;
    let batch = QConnectMessages {
        messages_time: Some(now_ms()),
        messages_id: Some(next_batch_seq()),
        messages: vec![message],
    };
    Ok(batch.encode_to_vec())
}

fn map_queue_command(command: &QueueCommand) -> Result<QConnectMessage, ProtocolError> {
    let queue_version_ref = Some(to_proto_queue_version(command.queue_version_ref)?);
    let action_uuid = Some(action_uuid_bytes(&command.action_uuid)?);

    match command.command_type {
        QueueCommandType::CtrlSrvrJoinSession => Ok(QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrJoinSession as i32),
            ctrl_srvr_join_session: Some(JoinSessionMessage {
                session_uuid: optional_uuid_bytes(
                    &command.payload,
                    &["session_uuid", "sessionUuid"],
                )?,
                device_info: parse_device_info(&command.payload)?,
                // Controller JoinSession doesn't use renderer-specific fields
                reason: None,
                initial_state: None,
                is_active: None,
            }),
            ..Default::default()
        }),
        QueueCommandType::CtrlSrvrSetPlayerState => Ok(QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrSetPlayerState as i32),
            ctrl_srvr_set_player_state: Some(SetPlayerStateMessage {
                playing_state: optional_i32_any(
                    &command.payload,
                    &["playing_state", "playingState"],
                )?,
                current_position: optional_i32_any(
                    &command.payload,
                    &["current_position", "currentPosition"],
                )?,
                current_queue_item: parse_set_player_state_queue_item(&command.payload)?,
            }),
            ..Default::default()
        }),
        QueueCommandType::CtrlSrvrSetActiveRenderer => Ok(QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrSetActiveRenderer as i32),
            ctrl_srvr_set_active_renderer: Some(SetActiveRendererMessage {
                renderer_id: optional_i32_any(&command.payload, &["renderer_id", "rendererId"])?,
            }),
            ..Default::default()
        }),
        QueueCommandType::CtrlSrvrSetVolume => Ok(QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrSetVolume as i32),
            ctrl_srvr_set_volume: Some(SetVolumeMessage {
                renderer_id: optional_i32_any(&command.payload, &["renderer_id", "rendererId"])?,
                volume: optional_i32_any(&command.payload, &["volume"])?,
                volume_delta: optional_i32_any(&command.payload, &["volume_delta", "volumeDelta"])?,
            }),
            ..Default::default()
        }),
        QueueCommandType::CtrlSrvrSetLoopMode => Ok(QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrSetLoopMode as i32),
            ctrl_srvr_set_loop_mode: Some(SetLoopModeMessage {
                loop_mode: optional_i32_any(&command.payload, &["loop_mode", "loopMode"])?,
            }),
            ..Default::default()
        }),
        QueueCommandType::CtrlSrvrMuteVolume => Ok(QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrMuteVolume as i32),
            ctrl_srvr_mute_volume: Some(MuteVolumeMessage {
                renderer_id: optional_i32_any(&command.payload, &["renderer_id", "rendererId"])?,
                value: Some(optional_bool_any(&command.payload, &["value"], false)),
            }),
            ..Default::default()
        }),
        QueueCommandType::CtrlSrvrSetMaxAudioQuality => Ok(QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrSetMaxAudioQuality as i32),
            ctrl_srvr_set_max_audio_quality: Some(SetMaxAudioQualityMessage {
                renderer_id: optional_i32_any(&command.payload, &["renderer_id", "rendererId"])?,
                max_audio_quality: optional_i32_any(
                    &command.payload,
                    &["max_audio_quality", "maxAudioQuality"],
                )?,
            }),
            ..Default::default()
        }),
        QueueCommandType::CtrlSrvrAskForRendererState => Ok(QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrAskForRendererState as i32),
            ctrl_srvr_ask_for_renderer_state: Some(AskForRendererStateMessage {
                renderer_id: optional_i32_any(&command.payload, &["renderer_id", "rendererId"])?,
            }),
            ..Default::default()
        }),
        QueueCommandType::CtrlSrvrQueueAddTracks => {
            let track_ids = required_u32_list(&command.payload, "track_ids")?;
            let shuffle_seed = optional_i32(&command.payload, "shuffle_seed")?.map(|v| v as u32);
            let context_uuid = optional_uuid_bytes(&command.payload, &["context_uuid"])?;
            let autoplay_reset = optional_bool(&command.payload, "autoplay_reset", false);
            let autoplay_loading =
                optional_bool(&command.payload, "autoplay_loading", autoplay_reset);

            Ok(QConnectMessage {
                message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrQueueAddTracks as i32),
                ctrl_srvr_queue_add_tracks: Some(QueueAddTracksMessage {
                    queue_version_ref,
                    action_uuid,
                    track_ids,
                    shuffle_seed,
                    context_uuid,
                    autoplay_reset: Some(autoplay_reset),
                    autoplay_loading: Some(autoplay_loading),
                }),
                ..Default::default()
            })
        }
        QueueCommandType::CtrlSrvrQueueLoadTracks => {
            let track_ids = required_u32_list(&command.payload, "track_ids")?;
            let queue_position = optional_i32(&command.payload, "queue_position")?;
            let shuffle_seed = optional_i32(&command.payload, "shuffle_seed")?.map(|v| v as u32);
            let shuffle_pivot_index = optional_i32(&command.payload, "shuffle_pivot_index")?;
            let shuffle_mode = optional_bool(&command.payload, "shuffle_mode", false);
            let context_uuid = optional_uuid_bytes(&command.payload, &["context_uuid"])?;
            let autoplay_reset = optional_bool(&command.payload, "autoplay_reset", false);
            let autoplay_loading = optional_bool(&command.payload, "autoplay_loading", false);

            Ok(QConnectMessage {
                message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrQueueLoadTracks as i32),
                ctrl_srvr_queue_load_tracks: Some(QueueLoadTracksMessage {
                    queue_version_ref,
                    action_uuid,
                    track_ids,
                    queue_position,
                    shuffle_seed,
                    shuffle_pivot_index,
                    shuffle_mode: Some(shuffle_mode),
                    context_uuid,
                    autoplay_reset: Some(autoplay_reset),
                    autoplay_loading: Some(autoplay_loading),
                }),
                ..Default::default()
            })
        }
        QueueCommandType::CtrlSrvrQueueInsertTracks => {
            let track_ids = required_u32_list(&command.payload, "track_ids")?;
            let insert_after = optional_i32(&command.payload, "insert_after")?;
            let shuffle_seed = optional_i32(&command.payload, "shuffle_seed")?.map(|v| v as u32);
            let context_uuid = optional_uuid_bytes(&command.payload, &["context_uuid"])?;
            let autoplay_reset = optional_bool(&command.payload, "autoplay_reset", false);
            let autoplay_loading =
                optional_bool(&command.payload, "autoplay_loading", autoplay_reset);

            Ok(QConnectMessage {
                message_type: Some(
                    QConnectMessageType::MessageTypeCtrlSrvrQueueInsertTracks as i32,
                ),
                ctrl_srvr_queue_insert_tracks: Some(QueueInsertTracksMessage {
                    queue_version_ref,
                    action_uuid,
                    track_ids,
                    insert_after,
                    shuffle_seed,
                    context_uuid,
                    autoplay_reset: Some(autoplay_reset),
                    autoplay_loading: Some(autoplay_loading),
                }),
                ..Default::default()
            })
        }
        QueueCommandType::CtrlSrvrQueueRemoveTracks => {
            let queue_item_ids = required_i32_list(&command.payload, "queue_item_ids")?;
            let autoplay_reset = optional_bool(&command.payload, "autoplay_reset", false);
            let autoplay_loading =
                optional_bool(&command.payload, "autoplay_loading", autoplay_reset);

            Ok(QConnectMessage {
                message_type: Some(
                    QConnectMessageType::MessageTypeCtrlSrvrQueueRemoveTracks as i32,
                ),
                ctrl_srvr_queue_remove_tracks: Some(QueueRemoveTracksMessage {
                    queue_version_ref,
                    action_uuid,
                    queue_item_ids,
                    autoplay_reset: Some(autoplay_reset),
                    autoplay_loading: Some(autoplay_loading),
                }),
                ..Default::default()
            })
        }
        QueueCommandType::CtrlSrvrQueueReorderTracks => {
            let queue_item_ids = required_i32_list(&command.payload, "queue_item_ids")?;
            let insert_after = optional_i32(&command.payload, "insert_after")?;
            let autoplay_reset = optional_bool(&command.payload, "autoplay_reset", false);
            let autoplay_loading =
                optional_bool(&command.payload, "autoplay_loading", autoplay_reset);

            Ok(QConnectMessage {
                message_type: Some(
                    QConnectMessageType::MessageTypeCtrlSrvrQueueReorderTracks as i32,
                ),
                ctrl_srvr_queue_reorder_tracks: Some(QueueReorderTracksMessage {
                    queue_version_ref,
                    action_uuid,
                    queue_item_ids,
                    insert_after,
                    autoplay_reset: Some(autoplay_reset),
                    autoplay_loading: Some(autoplay_loading),
                }),
                ..Default::default()
            })
        }
        QueueCommandType::CtrlSrvrClearQueue => Ok(QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrClearQueue as i32),
            ctrl_srvr_clear_queue: Some(ClearQueueMessage {
                queue_version_ref,
                action_uuid,
            }),
            ..Default::default()
        }),
        QueueCommandType::CtrlSrvrSetShuffleMode => {
            let shuffle_mode = optional_bool(&command.payload, "shuffle_mode", false);
            let shuffle_seed = optional_i32(&command.payload, "shuffle_seed")?.map(|v| v as u32);
            let shuffle_pivot_queue_item_id =
                optional_i32(&command.payload, "shuffle_pivot_queue_item_id")?;
            let autoplay_reset = optional_bool(&command.payload, "autoplay_reset", false);
            let autoplay_loading =
                optional_bool(&command.payload, "autoplay_loading", autoplay_reset);

            Ok(QConnectMessage {
                message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrSetShuffleMode as i32),
                ctrl_srvr_set_shuffle_mode: Some(SetShuffleModeMessage {
                    queue_version_ref,
                    action_uuid,
                    shuffle_mode: Some(shuffle_mode),
                    shuffle_seed,
                    shuffle_pivot_queue_item_id,
                    autoplay_reset: Some(autoplay_reset),
                    autoplay_loading: Some(autoplay_loading),
                }),
                ..Default::default()
            })
        }
        QueueCommandType::CtrlSrvrSetAutoplayMode => {
            let autoplay_mode = optional_bool(&command.payload, "autoplay_mode", false);
            // Android forces these defaults for setAutoplayMode.
            let autoplay_reset = optional_bool(&command.payload, "autoplay_reset", true);
            let autoplay_loading = optional_bool(&command.payload, "autoplay_loading", false);

            Ok(QConnectMessage {
                message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrSetAutoplayMode as i32),
                ctrl_srvr_set_autoplay_mode: Some(SetAutoplayModeMessage {
                    queue_version_ref,
                    action_uuid,
                    autoplay_mode: Some(autoplay_mode),
                    autoplay_reset: Some(autoplay_reset),
                    autoplay_loading: Some(autoplay_loading),
                }),
                ..Default::default()
            })
        }
        QueueCommandType::CtrlSrvrAutoplayLoadTracks => {
            let track_ids = required_u32_list(&command.payload, "track_ids")?;
            let context_uuid = optional_uuid_bytes(&command.payload, &["context_uuid"])?;

            Ok(QConnectMessage {
                message_type: Some(
                    QConnectMessageType::MessageTypeCtrlSrvrAutoplayLoadTracks as i32,
                ),
                ctrl_srvr_autoplay_load_tracks: Some(AutoplayLoadTracksMessage {
                    queue_version_ref,
                    action_uuid,
                    track_ids,
                    context_uuid,
                }),
                ..Default::default()
            })
        }
        QueueCommandType::CtrlSrvrAutoplayRemoveTracks => {
            let queue_item_ids = required_i32_list(&command.payload, "queue_item_ids")?;

            Ok(QConnectMessage {
                message_type: Some(
                    QConnectMessageType::MessageTypeCtrlSrvrAutoplayRemoveTracks as i32,
                ),
                ctrl_srvr_autoplay_remove_tracks: Some(AutoplayRemoveTracksMessage {
                    queue_version_ref,
                    action_uuid,
                    queue_item_ids,
                }),
                ..Default::default()
            })
        }
        QueueCommandType::CtrlSrvrSetQueueState => {
            let tracks = required_tracks_with_context(&command.payload, "tracks")?;
            let shuffle_mode = optional_bool(&command.payload, "shuffle_mode", false);
            let shuffled_track_indexes =
                optional_i32_list(&command.payload, "shuffled_track_indexes")?;
            let autoplay_mode = optional_bool(&command.payload, "autoplay_mode", false);
            let autoplay_loading = optional_bool(&command.payload, "autoplay_loading", false);
            let autoplay_tracks =
                required_tracks_with_context(&command.payload, "autoplay_tracks")?;

            Ok(QConnectMessage {
                message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrSetQueueState as i32),
                ctrl_srvr_set_queue_state: Some(SetQueueStateMessage {
                    queue_version_ref,
                    action_uuid,
                    tracks,
                    shuffle_mode: Some(shuffle_mode),
                    shuffled_track_indexes,
                    autoplay_mode: Some(autoplay_mode),
                    autoplay_loading: Some(autoplay_loading),
                    autoplay_tracks,
                }),
                ..Default::default()
            })
        }
        QueueCommandType::CtrlSrvrAskForQueueState => Ok(QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeCtrlSrvrAskForQueueState as i32),
            ctrl_srvr_ask_for_queue_state: Some(AskForQueueStateMessage {
                queue_version_ref,
                action_uuid,
            }),
            ..Default::default()
        }),
    }
}

fn map_renderer_report(report: &RendererReport) -> Result<QConnectMessage, ProtocolError> {
    match report.report_type {
        RendererReportType::RndrSrvrJoinSession => {
            let initial_state = parse_renderer_initial_state(&report.payload)?;
            let is_active = Some(optional_bool(&report.payload, "is_active", false));
            let reason = optional_i32(&report.payload, "reason")?;

            Ok(QConnectMessage {
                message_type: Some(QConnectMessageType::MessageTypeRndrSrvrJoinSession as i32),
                rndr_srvr_join_session: Some(JoinSessionMessage {
                    session_uuid: optional_uuid_bytes(
                        &report.payload,
                        &["session_uuid", "sessionUuid"],
                    )?,
                    device_info: parse_device_info(&report.payload)?,
                    reason,
                    initial_state,
                    is_active,
                }),
                ..Default::default()
            })
        }
        RendererReportType::RndrSrvrDeviceInfoUpdated => Ok(QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeRndrSrvrDeviceInfoUpdated as i32),
            rndr_srvr_device_info_updated: Some(RendererDeviceInfoUpdatedMessage {
                device_info: parse_device_info(&report.payload)?,
            }),
            ..Default::default()
        }),
        RendererReportType::RndrSrvrStateUpdated => {
            let queue_version = optional_queue_version(&report.payload, "queue_version")?
                .unwrap_or(report.queue_version_ref);
            let current_position = optional_i32(&report.payload, "current_position")?;
            let duration = optional_i32(&report.payload, "duration")?;
            let current_qid = normalize_renderer_state_queue_item_id(optional_i32(
                &report.payload,
                "current_queue_item_id",
            )?);
            let next_qid = normalize_renderer_state_queue_item_id(optional_i32(
                &report.payload,
                "next_queue_item_id",
            )?);
            let playing_state = optional_i32(&report.payload, "playing_state")?;
            let playback_position = current_position.map(|value| PlaybackPositionMessage {
                timestamp: Some(now_ms()),
                value: Some(value),
            });

            log::debug!(
                "[QConnect/Proto] Encoding StateUpdated: playing={:?} pos={:?} dur={:?} qid={:?} next_qid={:?} qv={}.{}",
                playing_state, current_position, duration,
                current_qid, next_qid,
                queue_version.major, queue_version.minor
            );

            Ok(QConnectMessage {
                message_type: Some(QConnectMessageType::MessageTypeRndrSrvrStateUpdated as i32),
                rndr_srvr_state_updated: Some(RendererStateUpdatedMessage {
                    state: Some(RendererStateMessage {
                        playing_state,
                        buffer_state: optional_i32(&report.payload, "buffer_state")?,
                        current_position: playback_position,
                        duration,
                        queue_version: Some(to_proto_queue_version(queue_version)?),
                        current_queue_item_id: current_qid,
                        next_queue_item_id: next_qid,
                    }),
                }),
                ..Default::default()
            })
        }
        RendererReportType::RndrSrvrVolumeChanged => Ok(QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeRndrSrvrVolumeChanged as i32),
            rndr_srvr_volume_changed: Some(RendererVolumeChangedMessage {
                volume: optional_i32(&report.payload, "volume")?,
            }),
            ..Default::default()
        }),
        RendererReportType::RndrSrvrVolumeMuted => Ok(QConnectMessage {
            message_type: Some(QConnectMessageType::MessageTypeRndrSrvrVolumeMuted as i32),
            rndr_srvr_volume_muted: Some(RendererVolumeMutedMessage {
                value: Some(optional_bool(&report.payload, "value", false)),
            }),
            ..Default::default()
        }),
        RendererReportType::RndrSrvrFileAudioQualityChanged => Ok(QConnectMessage {
            message_type: Some(
                QConnectMessageType::MessageTypeRndrSrvrFileAudioQualityChanged as i32,
            ),
            rndr_srvr_file_audio_quality_changed: Some(RendererFileAudioQualityChangedMessage {
                sampling_rate: optional_i32(&report.payload, "sampling_rate")?,
                bit_depth: optional_i32(&report.payload, "bit_depth")?,
                nb_channels: optional_i32(&report.payload, "nb_channels")?,
                audio_quality: optional_i32(&report.payload, "audio_quality")?,
            }),
            ..Default::default()
        }),
        RendererReportType::RndrSrvrDeviceAudioQualityChanged => Ok(QConnectMessage {
            message_type: Some(
                QConnectMessageType::MessageTypeRndrSrvrDeviceAudioQualityChanged as i32,
            ),
            rndr_srvr_device_audio_quality_changed: Some(
                RendererDeviceAudioQualityChangedMessage {
                    sampling_rate: optional_i32(&report.payload, "sampling_rate")?,
                    bit_depth: optional_i32(&report.payload, "bit_depth")?,
                    nb_channels: optional_i32(&report.payload, "nb_channels")?,
                },
            ),
            ..Default::default()
        }),
        RendererReportType::RndrSrvrMaxAudioQualityChanged => Ok(QConnectMessage {
            message_type: Some(
                QConnectMessageType::MessageTypeRndrSrvrMaxAudioQualityChanged as i32,
            ),
            rndr_srvr_max_audio_quality_changed: Some(RendererMaxAudioQualityChangedMessage {
                max_audio_quality: optional_i32(&report.payload, "max_audio_quality")?,
                network_type: optional_i32(&report.payload, "network_type")?,
            }),
            ..Default::default()
        }),
    }
}

fn normalize_renderer_state_queue_item_id(queue_item_id: Option<i32>) -> Option<i32> {
    match queue_item_id {
        Some(0) | None => None,
        other => other,
    }
}

fn to_proto_queue_version(
    version: qconnect_core::QueueVersion,
) -> Result<QueueVersionRef, ProtocolError> {
    Ok(QueueVersionRef {
        major: Some(to_i32_from_u64(version.major, "queue_version_ref.major")?),
        minor: Some(to_i32_from_u64(version.minor, "queue_version_ref.minor")?),
    })
}

fn action_uuid_bytes(value: &str) -> Result<Vec<u8>, ProtocolError> {
    let uuid = Uuid::parse_str(value).map_err(|err| {
        ProtocolError::InvalidUuid(format!("action_uuid '{}' parse error: {err}", value))
    })?;
    Ok(uuid.as_bytes().to_vec())
}

fn optional_uuid_bytes(payload: &Value, keys: &[&str]) -> Result<Option<Vec<u8>>, ProtocolError> {
    for key in keys {
        if let Some(raw) = payload.get(*key).and_then(Value::as_str) {
            let uuid = Uuid::parse_str(raw).map_err(|err| {
                ProtocolError::InvalidUuid(format!("{} '{}' parse error: {err}", key, raw))
            })?;
            return Ok(Some(uuid.as_bytes().to_vec()));
        }
    }
    Ok(None)
}

fn required_i32_list(payload: &Value, key: &str) -> Result<Vec<i32>, ProtocolError> {
    let values = payload
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| ProtocolError::InvalidPayload(format!("missing array field '{key}'")))?;

    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let raw = value
                .as_i64()
                .or_else(|| value.as_u64().map(|v| v as i64))
                .ok_or_else(|| {
                    ProtocolError::InvalidPayload(format!("field '{key}[{idx}]' is not numeric"))
                })?;
            to_i32_from_i64(raw, &format!("{key}[{idx}]"))
        })
        .collect()
}

fn required_u32_list(payload: &Value, key: &str) -> Result<Vec<u32>, ProtocolError> {
    let values = payload
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| ProtocolError::InvalidPayload(format!("missing array field '{key}'")))?;

    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let raw = value
                .as_i64()
                .or_else(|| value.as_u64().map(|v| v as i64))
                .ok_or_else(|| {
                    ProtocolError::InvalidPayload(format!("field '{key}[{idx}]' is not numeric"))
                })?;
            u32::try_from(raw).map_err(|_| {
                ProtocolError::InvalidPayload(format!(
                    "field '{key}[{idx}]' value {raw} out of u32 range"
                ))
            })
        })
        .collect()
}

fn optional_i32_list(payload: &Value, key: &str) -> Result<Vec<i32>, ProtocolError> {
    let Some(values) = payload.get(key).and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let raw = value
                .as_i64()
                .or_else(|| value.as_u64().map(|v| v as i64))
                .ok_or_else(|| {
                    ProtocolError::InvalidPayload(format!("field '{key}[{idx}]' is not numeric"))
                })?;
            to_i32_from_i64(raw, &format!("{key}[{idx}]"))
        })
        .collect()
}

fn required_tracks_with_context(
    payload: &Value,
    key: &str,
) -> Result<Vec<SetQueueTrackWithContext>, ProtocolError> {
    let Some(values) = payload.get(key).and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let track_id = value
                .get("track_id")
                .or_else(|| value.get("trackId"))
                .and_then(Value::as_i64)
                .or_else(|| {
                    value
                        .get("track_id")
                        .or_else(|| value.get("trackId"))
                        .and_then(Value::as_u64)
                        .map(|v| v as i64)
                })
                .ok_or_else(|| {
                    ProtocolError::InvalidPayload(format!(
                        "field '{key}[{idx}].track_id' is required"
                    ))
                })?;

            let context_uuid = value
                .get("context_uuid")
                .or_else(|| value.get("track_context_uuid"))
                .and_then(Value::as_str)
                .map(parse_uuid)
                .transpose()?;

            Ok(SetQueueTrackWithContext {
                track_id: Some(to_i32_from_i64(
                    track_id,
                    &format!("{key}[{idx}].track_id"),
                )?),
                context_uuid,
            })
        })
        .collect()
}

fn optional_queue_version(
    payload: &Value,
    key: &str,
) -> Result<Option<qconnect_core::QueueVersion>, ProtocolError> {
    let Some(version) = payload.get(key) else {
        return Ok(None);
    };

    let Some(major) = version.get("major").and_then(Value::as_u64) else {
        return Ok(None);
    };
    let Some(minor) = version.get("minor").and_then(Value::as_u64) else {
        return Ok(None);
    };

    Ok(Some(qconnect_core::QueueVersion { major, minor }))
}

fn optional_i32(payload: &Value, key: &str) -> Result<Option<i32>, ProtocolError> {
    match payload.get(key) {
        None => Ok(None),
        Some(value) if value.is_null() => Ok(None),
        Some(value) => {
            let raw = value
                .as_i64()
                .or_else(|| value.as_u64().map(|v| v as i64))
                .ok_or_else(|| {
                    ProtocolError::InvalidPayload(format!("field '{key}' is not numeric"))
                })?;
            Ok(Some(to_i32_from_i64(raw, key)?))
        }
    }
}

fn optional_i32_any(payload: &Value, keys: &[&str]) -> Result<Option<i32>, ProtocolError> {
    for key in keys {
        if payload.get(*key).is_some() {
            return optional_i32(payload, key);
        }
    }

    Ok(None)
}

fn optional_bool(payload: &Value, key: &str, default: bool) -> bool {
    payload.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn optional_bool_any(payload: &Value, keys: &[&str], default: bool) -> bool {
    for key in keys {
        let Some(value) = payload.get(*key) else {
            continue;
        };

        if let Some(as_bool) = value.as_bool() {
            return as_bool;
        }
        if let Some(as_int) = value.as_i64() {
            return as_int != 0;
        }
        if let Some(as_uint) = value.as_u64() {
            return as_uint != 0;
        }
    }

    default
}

fn optional_string_any(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .map(ToString::to_string)
}

fn parse_queue_version_ref(payload: &Value, key: &str) -> Option<QueueVersionRef> {
    let version = payload.get(key)?;
    let major = version
        .get("major")
        .and_then(Value::as_u64)
        .and_then(|value| i32::try_from(value).ok())?;
    let minor = version
        .get("minor")
        .and_then(Value::as_u64)
        .and_then(|value| i32::try_from(value).ok())?;

    Some(QueueVersionRef {
        major: Some(major),
        minor: Some(minor),
    })
}

fn parse_device_info(payload: &Value) -> Result<Option<DeviceInfoMessage>, ProtocolError> {
    let nested = payload
        .get("device_info")
        .or_else(|| payload.get("deviceInfo"));
    let source = match nested {
        Some(value) => value,
        None => payload,
    };

    if !source.is_object() {
        return Err(ProtocolError::InvalidPayload(
            "field 'device_info' must be an object".to_string(),
        ));
    }

    let device_uuid = optional_uuid_bytes(source, &["device_uuid", "deviceUuid", "uuid"])?;
    let friendly_name = optional_string_any(source, &["friendly_name", "friendlyName"]);
    let brand = optional_string_any(source, &["brand"]);
    let model = optional_string_any(source, &["model"]);
    let serial_number = optional_string_any(source, &["serial_number", "serialNumber"]);
    let device_type = optional_i32_any(source, &["device_type", "deviceType"])?;
    let software_version = optional_string_any(source, &["software_version", "softwareVersion"]);
    let capabilities = parse_device_capabilities(source)?;

    if device_uuid.is_none()
        && friendly_name.is_none()
        && brand.is_none()
        && model.is_none()
        && serial_number.is_none()
        && device_type.is_none()
        && software_version.is_none()
        && capabilities.is_none()
    {
        return Ok(None);
    }

    Ok(Some(DeviceInfoMessage {
        device_uuid,
        friendly_name,
        brand,
        model,
        serial_number,
        device_type,
        capabilities,
        software_version,
    }))
}

fn parse_device_capabilities(
    source: &Value,
) -> Result<Option<DeviceCapabilitiesMessage>, ProtocolError> {
    let nested = source
        .get("capabilities")
        .or_else(|| source.get("deviceCapabilities"));

    if let Some(caps) = nested {
        if caps.is_object() {
            return Ok(Some(DeviceCapabilitiesMessage {
                min_audio_quality: optional_i32_any(
                    caps,
                    &["min_audio_quality", "minAudioQuality"],
                )?,
                max_audio_quality: optional_i32_any(
                    caps,
                    &["max_audio_quality", "maxAudioQuality"],
                )?,
                volume_remote_control: optional_i32_any(
                    caps,
                    &["volume_remote_control", "volumeRemoteControl"],
                )?,
            }));
        }
    }

    // Check flat fields (capabilities declared directly on device_info payload)
    let min_aq = optional_i32_any(source, &["min_audio_quality", "minAudioQuality"])?;
    let max_aq = optional_i32_any(source, &["max_audio_quality", "maxAudioQuality"])?;
    let vol_rc = optional_i32_any(source, &["volume_remote_control", "volumeRemoteControl"])?;

    if min_aq.is_some() || max_aq.is_some() || vol_rc.is_some() {
        return Ok(Some(DeviceCapabilitiesMessage {
            min_audio_quality: min_aq,
            max_audio_quality: max_aq,
            volume_remote_control: vol_rc,
        }));
    }

    Ok(None)
}

fn parse_renderer_initial_state(
    payload: &Value,
) -> Result<Option<RendererStateMessage>, ProtocolError> {
    let nested = payload
        .get("initial_state")
        .or_else(|| payload.get("initialState"));

    let Some(source) = nested else {
        return Ok(None);
    };

    if !source.is_object() {
        return Ok(None);
    }

    let current_position = optional_i32(source, "current_position")?;
    let playback_position = current_position.map(|value| PlaybackPositionMessage {
        timestamp: Some(now_ms()),
        value: Some(value),
    });

    Ok(Some(RendererStateMessage {
        playing_state: optional_i32(source, "playing_state")?,
        buffer_state: optional_i32(source, "buffer_state")?,
        current_position: playback_position,
        duration: optional_i32(source, "duration")?,
        queue_version: optional_queue_version(source, "queue_version")?
            .map(|v| to_proto_queue_version(v))
            .transpose()?,
        current_queue_item_id: optional_i32(source, "current_queue_item_id")?,
        next_queue_item_id: optional_i32(source, "next_queue_item_id")?,
    }))
}

fn parse_set_player_state_queue_item(
    payload: &Value,
) -> Result<Option<SetPlayerStateQueueItemMessage>, ProtocolError> {
    let nested = payload
        .get("current_queue_item")
        .or_else(|| payload.get("currentQueueItem"));
    // A present-but-null `current_queue_item` means "no item" — a bare play/pause
    // toggle. Treat it as ABSENT rather than hard-failing the whole command encode
    // (the old behavior aborted the send before it reached the wire). Defense in
    // depth alongside `skip_serializing_if` on the request struct.
    if matches!(nested, Some(Value::Null)) {
        return Ok(None);
    }
    let source = match nested {
        Some(value) => value,
        None => payload,
    };

    if !source.is_object() {
        return Err(ProtocolError::InvalidPayload(
            "field 'current_queue_item' must be an object".to_string(),
        ));
    }

    let queue_version = parse_queue_version_ref(source, "queue_version")
        .or_else(|| parse_queue_version_ref(source, "queueVersion"));
    let id = optional_i32_any(source, &["id", "queue_item_id", "queueItemId"])?;

    if queue_version.is_none() && id.is_none() {
        return Ok(None);
    }

    Ok(Some(SetPlayerStateQueueItemMessage { queue_version, id }))
}

fn parse_uuid(value: &str) -> Result<Vec<u8>, ProtocolError> {
    let parsed = Uuid::parse_str(value)
        .map_err(|err| ProtocolError::InvalidUuid(format!("'{value}' parse error: {err}")))?;
    Ok(parsed.as_bytes().to_vec())
}

fn to_i32_from_u64(value: u64, field_name: &str) -> Result<i32, ProtocolError> {
    i32::try_from(value).map_err(|_| {
        ProtocolError::InvalidPayload(format!("field '{field_name}' is out of i32 range: {value}"))
    })
}

fn to_i32_from_i64(value: i64, field_name: &str) -> Result<i32, ProtocolError> {
    i32::try_from(value).map_err(|_| {
        ProtocolError::InvalidPayload(format!("field '{field_name}' is out of i32 range: {value}"))
    })
}

fn next_batch_seq() -> i32 {
    BATCH_SEQ.fetch_add(1, Ordering::Relaxed)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue_command_proto::{QConnectMessageType, QConnectMessages};
    use crate::{QueueCommand, RendererReport, RendererReportType};
    use prost::Message;
    use qconnect_core::QueueVersion;
    use serde_json::json;

    #[test]
    fn device_info_update_matches_official_nested_wire_shape() {
        // Official Renderer.DeviceInfoUpdatedMessage: device_info at tag 1;
        // Common.DeviceInfo: capabilities at tag 7; VolumeRemoteControl at tag 3.
        // Fixed bytes keep this independent of our protobuf decoder's schema.
        for permission in [1u8, 2] {
            for nested in [false, true] {
                let info = json!({"capabilities": {"volume_remote_control": permission}});
                let payload = if nested { json!({"device_info": info}) } else { info };
                let report = RendererReport::new(
                    RendererReportType::RndrSrvrDeviceInfoUpdated,
                    "00000000-0000-0000-0000-000000000000",
                    QueueVersion::default(), payload,
                );
                assert_eq!(map_renderer_report(&report).unwrap().encode_to_vec(),
                    vec![0x08, 22, 0xb2, 0x01, 6, 0x0a, 4, 0x3a, 2, 0x18, permission]);
            }
        }
    }

    #[test]
    fn maps_device_audio_quality_changed_report_to_tag_27() {
        let report = RendererReport::new(
            RendererReportType::RndrSrvrDeviceAudioQualityChanged,
            "00000000-0000-0000-0000-000000000000",
            QueueVersion { major: 1, minor: 2 },
            json!({ "sampling_rate": 96000, "bit_depth": 24, "nb_channels": 2 }),
        );
        let msg = map_renderer_report(&report).expect("mapping should succeed");
        assert_eq!(
            msg.message_type,
            Some(QConnectMessageType::MessageTypeRndrSrvrDeviceAudioQualityChanged as i32)
        );
        let dev = msg
            .rndr_srvr_device_audio_quality_changed
            .expect("device AQ submessage present");
        assert_eq!(dev.sampling_rate, Some(96000));
        assert_eq!(dev.bit_depth, Some(24));
        assert_eq!(dev.nb_channels, Some(2));
        assert!(msg.rndr_srvr_file_audio_quality_changed.is_none());
        assert!(msg.rndr_srvr_max_audio_quality_changed.is_none());
    }

    #[test]
    fn unknown_device_depth_remains_absent_on_the_wire() {
        let report = RendererReport::new(
            RendererReportType::RndrSrvrDeviceAudioQualityChanged,
            "00000000-0000-0000-0000-000000000000",
            QueueVersion::default(),
            json!({ "sampling_rate": 48000, "bit_depth": null, "nb_channels": 2 }),
        );
        let dev = map_renderer_report(&report)
            .unwrap()
            .rndr_srvr_device_audio_quality_changed
            .unwrap();
        assert_eq!(dev.sampling_rate, Some(48000));
        assert_eq!(dev.bit_depth, None);
        assert_eq!(dev.nb_channels, Some(2));
    }

    #[test]
    fn encodes_add_tracks_command_into_binary_batch() {
        let command = QueueCommand::new(
            QueueCommandType::CtrlSrvrQueueAddTracks,
            "85fa0dd6-7bd6-4b3c-8f43-b8ee22e65d5e",
            QueueVersion::new(1, 2),
            json!({
                "track_ids": [101, 102],
                "context_uuid": "0f892e1a-a2f4-4d18-82c6-31e8daf2ea0f",
                "autoplay_reset": true
            }),
        );

        let payload = encode_queue_command_batch(&command).expect("batch payload");
        assert!(!payload.is_empty());
    }

    #[test]
    fn rejects_non_uuid_action_id() {
        let command = QueueCommand::new(
            QueueCommandType::CtrlSrvrQueueAddTracks,
            "not-a-uuid",
            QueueVersion::new(1, 2),
            json!({"track_ids": [1]}),
        );

        let err = encode_queue_command_batch(&command).expect_err("expected invalid uuid");
        assert!(matches!(err, ProtocolError::InvalidUuid(_)));
    }

    #[test]
    fn set_autoplay_mode_defaults_follow_android_behavior() {
        let command = QueueCommand::new(
            QueueCommandType::CtrlSrvrSetAutoplayMode,
            "2d8292c8-4f23-40f3-98a4-e3899eb0d03a",
            QueueVersion::new(7, 8),
            json!({"autoplay_mode": true}),
        );

        let payload = encode_queue_command_batch(&command).expect("batch payload");
        assert!(!payload.is_empty());
    }

    #[test]
    fn encodes_join_session_message_with_device_info() {
        let command = QueueCommand::new(
            QueueCommandType::CtrlSrvrJoinSession,
            "a043f34c-15f5-44bb-a5ad-06ce4e0c009d",
            QueueVersion::new(1, 0),
            json!({
                "session_uuid": "a3f7be0f-3854-44c6-9194-c93cedec322a",
                "device_info": {
                    "device_uuid": "3c2da4ec-f4b9-4600-9177-7f8a44ce69f4",
                    "friendly_name": "QBZ Desktop",
                    "brand": "QBZ",
                    "model": "Linux",
                    "serial_number": "qbz-dev-1",
                    "device_type": 6,
                    "software_version": "0.1.0"
                }
            }),
        );

        let payload = encode_queue_command_batch(&command).expect("batch payload");
        let decoded = QConnectMessages::decode(payload.as_slice()).expect("decode batch");
        assert_eq!(decoded.messages.len(), 1);

        let message = &decoded.messages[0];
        assert_eq!(
            message.message_type,
            Some(QConnectMessageType::MessageTypeCtrlSrvrJoinSession as i32)
        );
        let join = message
            .ctrl_srvr_join_session
            .as_ref()
            .expect("join payload");
        assert!(join.session_uuid.is_some());
        let device = join.device_info.as_ref().expect("device_info payload");
        assert_eq!(device.friendly_name.as_deref(), Some("QBZ Desktop"));
        assert_eq!(device.device_type, Some(6));
    }

    #[test]
    fn encodes_set_player_state_with_current_queue_item() {
        let command = QueueCommand::new(
            QueueCommandType::CtrlSrvrSetPlayerState,
            "132db70b-d293-4576-bfd6-747c40ab764f",
            QueueVersion::new(5, 3),
            json!({
                "playing_state": 2,
                "current_position": 42123,
                "current_queue_item": {
                    "queue_version": {
                        "major": 5,
                        "minor": 3
                    },
                    "id": 901
                }
            }),
        );

        let payload = encode_queue_command_batch(&command).expect("batch payload");
        let decoded = QConnectMessages::decode(payload.as_slice()).expect("decode batch");
        assert_eq!(decoded.messages.len(), 1);

        let message = &decoded.messages[0];
        assert_eq!(
            message.message_type,
            Some(QConnectMessageType::MessageTypeCtrlSrvrSetPlayerState as i32)
        );
        let player_state = message
            .ctrl_srvr_set_player_state
            .as_ref()
            .expect("player state payload");
        assert_eq!(player_state.playing_state, Some(2));
        assert_eq!(player_state.current_position, Some(42_123));
        let queue_item = player_state
            .current_queue_item
            .as_ref()
            .expect("current queue item payload");
        assert_eq!(queue_item.id, Some(901));
        assert_eq!(
            queue_item.queue_version.as_ref().and_then(|v| v.major),
            Some(5)
        );
        assert_eq!(
            queue_item.queue_version.as_ref().and_then(|v| v.minor),
            Some(3)
        );
    }

    #[test]
    fn encodes_bare_set_player_state_pause_without_queue_item() {
        // A bare play/pause toggle sends ONLY playing_state — the renderer
        // pauses/resumes its current item in place. Two shapes must both encode
        // cleanly with NO current_queue_item: the key omitted (via
        // skip_serializing_if), and present-but-null (defense in depth). The old
        // mapper rejected the null shape ("must be an object") and aborted the
        // whole send, which is why pause-to-an-iOS-renderer silently failed.
        for payload in [
            json!({ "playing_state": 3 }),
            json!({ "playing_state": 3, "current_position": null, "current_queue_item": null }),
        ] {
            let command = QueueCommand::new(
                QueueCommandType::CtrlSrvrSetPlayerState,
                "132db70b-d293-4576-bfd6-747c40ab764f",
                QueueVersion::new(5, 3),
                payload,
            );
            let encoded =
                encode_queue_command_batch(&command).expect("bare pause must encode (not abort)");
            let decoded = QConnectMessages::decode(encoded.as_slice()).expect("decode batch");
            let player_state = decoded.messages[0]
                .ctrl_srvr_set_player_state
                .as_ref()
                .expect("player state payload");
            assert_eq!(player_state.playing_state, Some(3));
            assert!(
                player_state.current_queue_item.is_none(),
                "bare pause must carry NO current_queue_item (qid 0 is the placeholder iOS rejects)"
            );
        }
    }

    #[test]
    fn encodes_queue_load_tracks_with_android_like_fields() {
        let command = QueueCommand::new(
            QueueCommandType::CtrlSrvrQueueLoadTracks,
            "f997cd72-d2ce-4121-a89b-75bf2be251ec",
            QueueVersion::new(12, 4),
            json!({
                "track_ids": [2001, 2002, 2003],
                "queue_position": 0,
                "shuffle_mode": true,
                "shuffle_seed": 3344,
                "shuffle_pivot_index": 1,
                "context_uuid": "a3f7be0f-3854-44c6-9194-c93cedec322a",
                "autoplay_reset": true
            }),
        );

        let payload = encode_queue_command_batch(&command).expect("batch payload");
        let decoded = QConnectMessages::decode(payload.as_slice()).expect("decode batch");
        assert_eq!(decoded.messages.len(), 1);

        let message = &decoded.messages[0];
        assert_eq!(
            message.message_type,
            Some(QConnectMessageType::MessageTypeCtrlSrvrQueueLoadTracks as i32)
        );

        let load = message
            .ctrl_srvr_queue_load_tracks
            .as_ref()
            .expect("queue load payload");
        assert_eq!(load.track_ids, vec![2001, 2002, 2003]);
        assert_eq!(load.queue_position, Some(0));
        assert_eq!(load.shuffle_mode, Some(true));
        assert_eq!(load.shuffle_seed, Some(3344));
        assert_eq!(load.shuffle_pivot_index, Some(1));
        assert_eq!(load.autoplay_reset, Some(true));
        assert_eq!(load.autoplay_loading, Some(false));
        assert_eq!(
            load.queue_version_ref
                .as_ref()
                .and_then(|version| version.major),
            Some(12)
        );
        assert_eq!(
            load.queue_version_ref
                .as_ref()
                .and_then(|version| version.minor),
            Some(4)
        );
        assert_eq!(load.context_uuid.as_ref().map(|uuid| uuid.len()), Some(16));
    }

    #[test]
    fn defaults_queue_load_tracks_autoplay_loading_to_false_when_omitted() {
        let command = QueueCommand::new(
            QueueCommandType::CtrlSrvrQueueLoadTracks,
            "f997cd72-d2ce-4121-a89b-75bf2be251ec",
            QueueVersion::new(3, 9),
            json!({
                "track_ids": [2001, 2002, 2003],
                "queue_position": 2,
                "shuffle_mode": false,
                "shuffle_pivot_index": 2,
                "context_uuid": "a3f7be0f-3854-44c6-9194-c93cedec322a",
                "autoplay_reset": true
            }),
        );

        let payload = encode_queue_command_batch(&command).expect("batch payload");
        let decoded = QConnectMessages::decode(payload.as_slice()).expect("decode batch");
        let load = decoded.messages[0]
            .ctrl_srvr_queue_load_tracks
            .as_ref()
            .expect("queue load payload");

        assert_eq!(load.queue_position, Some(2));
        assert_eq!(load.shuffle_mode, Some(false));
        assert_eq!(load.shuffle_pivot_index, Some(2));
        assert_eq!(load.autoplay_reset, Some(true));
        assert_eq!(load.autoplay_loading, Some(false));
        assert_eq!(load.context_uuid.as_ref().map(|uuid| uuid.len()), Some(16));
    }

    #[test]
    fn encodes_renderer_state_updated_report_with_queue_version() {
        let report = RendererReport::new(
            RendererReportType::RndrSrvrStateUpdated,
            "6d8ef3af-b863-4581-9b72-17bd32792c6d",
            QueueVersion::new(9, 4),
            json!({
                "playing_state": 2,
                "buffer_state": 2,
                "current_position": 42123,
                "duration": 185000,
                "current_queue_item_id": 9002,
                "next_queue_item_id": 9003
            }),
        );

        let payload = encode_renderer_report_batch(&report).expect("renderer report batch");
        let decoded = QConnectMessages::decode(payload.as_slice()).expect("decode batch");
        assert_eq!(decoded.messages.len(), 1);
        let message = &decoded.messages[0];
        assert_eq!(
            message.message_type,
            Some(QConnectMessageType::MessageTypeRndrSrvrStateUpdated as i32)
        );

        let state = message
            .rndr_srvr_state_updated
            .as_ref()
            .and_then(|payload| payload.state.as_ref())
            .expect("state payload");
        assert_eq!(state.playing_state, Some(2));
        assert_eq!(state.buffer_state, Some(2));
        assert_eq!(
            state.current_position.as_ref().and_then(|pos| pos.value),
            Some(42_123)
        );
        assert_eq!(state.queue_version.as_ref().and_then(|v| v.major), Some(9));
        assert_eq!(state.queue_version.as_ref().and_then(|v| v.minor), Some(4));
        assert_eq!(state.current_queue_item_id, Some(9002));
        assert_eq!(state.next_queue_item_id, Some(9003));
    }

    #[test]
    fn encodes_renderer_state_updated_omitting_zero_queue_item_ids() {
        let report = RendererReport::new(
            RendererReportType::RndrSrvrStateUpdated,
            "9b7db27a-efac-4fc4-9f22-6034b4444697",
            QueueVersion::new(7, 2),
            json!({
                "playing_state": 2,
                "buffer_state": 2,
                "current_position": 8000,
                "duration": 679000,
                "current_queue_item_id": 0,
                "next_queue_item_id": 18
            }),
        );

        let payload = encode_renderer_report_batch(&report).expect("renderer report batch");
        let decoded = QConnectMessages::decode(payload.as_slice()).expect("decode batch");
        let message = &decoded.messages[0];
        let state = message
            .rndr_srvr_state_updated
            .as_ref()
            .and_then(|payload| payload.state.as_ref())
            .expect("state payload");

        assert_eq!(state.current_queue_item_id, None);
        assert_eq!(state.next_queue_item_id, Some(18));
    }

    #[test]
    fn build_renderer_outbound_envelope_uses_renderer_message_type() {
        let report = RendererReport::new(
            RendererReportType::RndrSrvrVolumeChanged,
            "0f892e1a-a2f4-4d18-82c6-31e8daf2ea0f",
            QueueVersion::new(5, 6),
            json!({"volume": 58}),
        );

        let envelope =
            build_qconnect_renderer_outbound_envelope(report).expect("renderer envelope");
        assert_eq!(
            envelope.message_type,
            "MESSAGE_TYPE_RNDR_SRVR_VOLUME_CHANGED"
        );
        assert!(envelope.payload_bytes.is_some());
    }
}
