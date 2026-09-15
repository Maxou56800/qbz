#[derive(Clone, Copy, PartialEq, Eq, Debug, ::prost::Enumeration)]
#[repr(i32)]
pub enum QConnectMessageType {
    MessageTypeRndrSrvrJoinSession = 21,
    MessageTypeRndrSrvrDeviceInfoUpdated = 22,
    MessageTypeRndrSrvrStateUpdated = 23,
    MessageTypeRndrSrvrRendererAction = 24,
    MessageTypeRndrSrvrVolumeChanged = 25,
    MessageTypeRndrSrvrFileAudioQualityChanged = 26,
    MessageTypeRndrSrvrDeviceAudioQualityChanged = 27,
    MessageTypeRndrSrvrMaxAudioQualityChanged = 28,
    MessageTypeRndrSrvrVolumeMuted = 29,
    MessageTypeSrvrRndrSetState = 41,
    MessageTypeSrvrRndrSetVolume = 42,
    MessageTypeSrvrRndrSetActive = 43,
    MessageTypeSrvrRndrSetMaxAudioQuality = 44,
    MessageTypeSrvrRndrSetLoopMode = 45,
    MessageTypeSrvrRndrSetShuffleMode = 46,
    MessageTypeSrvrRndrMuteVolume = 47,
    MessageTypeCtrlSrvrJoinSession = 61,
    MessageTypeCtrlSrvrSetPlayerState = 62,
    MessageTypeCtrlSrvrSetActiveRenderer = 63,
    MessageTypeCtrlSrvrSetVolume = 64,
    MessageTypeCtrlSrvrClearQueue = 65,
    MessageTypeCtrlSrvrQueueLoadTracks = 66,
    MessageTypeCtrlSrvrQueueInsertTracks = 67,
    MessageTypeCtrlSrvrQueueAddTracks = 68,
    MessageTypeCtrlSrvrQueueRemoveTracks = 69,
    MessageTypeCtrlSrvrQueueReorderTracks = 70,
    MessageTypeCtrlSrvrSetShuffleMode = 71,
    MessageTypeCtrlSrvrSetLoopMode = 72,
    MessageTypeCtrlSrvrMuteVolume = 73,
    MessageTypeCtrlSrvrSetMaxAudioQuality = 74,
    MessageTypeCtrlSrvrSetQueueState = 75,
    MessageTypeCtrlSrvrAskForQueueState = 76,
    MessageTypeCtrlSrvrAskForRendererState = 77,
    MessageTypeCtrlSrvrSetAutoplayMode = 78,
    MessageTypeCtrlSrvrAutoplayLoadTracks = 79,
    MessageTypeCtrlSrvrAutoplayRemoveTracks = 80,
    MessageTypeSrvrCtrlSessionState = 81,
    MessageTypeSrvrCtrlRendererStateUpdated = 82,
    MessageTypeSrvrCtrlAddRenderer = 83,
    MessageTypeSrvrCtrlUpdateRenderer = 84,
    MessageTypeSrvrCtrlRemoveRenderer = 85,
    MessageTypeSrvrCtrlActiveRendererChanged = 86,
    MessageTypeSrvrCtrlVolumeChanged = 87,
    MessageTypeSrvrCtrlQueueErrorMessage = 88,
    MessageTypeSrvrCtrlQueueCleared = 89,
    MessageTypeSrvrCtrlQueueState = 90,
    MessageTypeSrvrCtrlQueueTracksLoaded = 91,
    MessageTypeSrvrCtrlQueueTracksInserted = 92,
    MessageTypeSrvrCtrlQueueTracksAdded = 93,
    MessageTypeSrvrCtrlQueueTracksRemoved = 94,
    MessageTypeSrvrCtrlQueueTracksReordered = 95,
    MessageTypeSrvrCtrlShuffleModeSet = 96,
    MessageTypeSrvrCtrlLoopModeSet = 97,
    MessageTypeSrvrCtrlVolumeMuted = 98,
    MessageTypeSrvrCtrlMaxAudioQualityChanged = 99,
    MessageTypeSrvrCtrlFileAudioQualityChanged = 100,
    MessageTypeSrvrCtrlDeviceAudioQualityChanged = 101,
    MessageTypeSrvrCtrlAutoplayModeSet = 102,
    MessageTypeSrvrCtrlAutoplayTracksLoaded = 103,
    MessageTypeSrvrCtrlAutoplayTracksRemoved = 104,
    MessageTypeSrvrCtrlQueueTracksAddedFromAutoplay = 105,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueVersionRef {
    #[prost(int32, optional, tag = "1")]
    pub major: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub minor: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ErrorMessage {
    /// Server sends error codes as strings (e.g. "ERROR_QUEUE_INSERT_TRACKS"),
    /// not integers. Wire type = LengthDelimited (string).
    #[prost(string, optional, tag = "1")]
    pub code: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub message: Option<String>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueTrack {
    #[prost(int32, optional, tag = "1")]
    pub queue_item_id: Option<i32>,
    #[prost(fixed32, optional, tag = "2")]
    pub track_id: Option<u32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueTrackWithContext {
    #[prost(int32, optional, tag = "1")]
    pub queue_item_id: Option<i32>,
    #[prost(fixed32, optional, tag = "2")]
    pub track_id: Option<u32>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub context_uuid: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetQueueTrackWithContext {
    #[prost(int32, optional, tag = "1")]
    pub track_id: Option<i32>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub context_uuid: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct DeviceCapabilitiesMessage {
    #[prost(int32, optional, tag = "1")]
    pub min_audio_quality: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub max_audio_quality: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub volume_remote_control: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct DeviceInfoMessage {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub device_uuid: Option<Vec<u8>>,
    #[prost(string, optional, tag = "2")]
    pub friendly_name: Option<String>,
    #[prost(string, optional, tag = "3")]
    pub brand: Option<String>,
    #[prost(string, optional, tag = "4")]
    pub model: Option<String>,
    #[prost(string, optional, tag = "5")]
    pub serial_number: Option<String>,
    #[prost(int32, optional, tag = "6")]
    pub device_type: Option<i32>,
    #[prost(message, optional, tag = "7")]
    pub capabilities: Option<DeviceCapabilitiesMessage>,
    #[prost(string, optional, tag = "8")]
    pub software_version: Option<String>,
}

/// Renderer.DeviceInfoUpdatedMessage wraps Common.DeviceInfo at field 1.
/// It is not wire-compatible with DeviceInfo itself (whose field 1 is a UUID).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererDeviceInfoUpdatedMessage {
    #[prost(message, optional, tag = "1")]
    pub device_info: Option<DeviceInfoMessage>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct JoinSessionMessage {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub session_uuid: Option<Vec<u8>>,
    #[prost(message, optional, tag = "2")]
    pub device_info: Option<DeviceInfoMessage>,
    /// Renderer-only: reason for joining (0=unknown, 1=controller_request, 2=reconnection)
    #[prost(int32, optional, tag = "3")]
    pub reason: Option<i32>,
    /// Renderer-only: initial playback state sent on join
    #[prost(message, optional, tag = "4")]
    pub initial_state: Option<RendererStateMessage>,
    /// Renderer-only: whether this renderer is active
    #[prost(bool, optional, tag = "5")]
    pub is_active: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetPlayerStateQueueItemMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(int32, optional, tag = "2")]
    pub id: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetPlayerStateMessage {
    #[prost(int32, optional, tag = "1")]
    pub playing_state: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub current_position: Option<i32>,
    #[prost(message, optional, tag = "3")]
    pub current_queue_item: Option<SetPlayerStateQueueItemMessage>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetActiveRendererMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetVolumeMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub volume: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub volume_delta: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetLoopModeMessage {
    #[prost(int32, optional, tag = "1")]
    pub loop_mode: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct MuteVolumeMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
    #[prost(bool, optional, tag = "2")]
    pub value: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetMaxAudioQualityMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub max_audio_quality: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AskForRendererStateMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ClearQueueMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version_ref: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueLoadTracksMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version_ref: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(fixed32, repeated, tag = "3")]
    pub track_ids: Vec<u32>,
    #[prost(int32, optional, tag = "4")]
    pub queue_position: Option<i32>,
    #[prost(fixed32, optional, tag = "5")]
    pub shuffle_seed: Option<u32>,
    #[prost(int32, optional, tag = "6")]
    pub shuffle_pivot_index: Option<i32>,
    #[prost(bool, optional, tag = "7")]
    pub shuffle_mode: Option<bool>,
    #[prost(bytes = "vec", optional, tag = "8")]
    pub context_uuid: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "9")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "10")]
    pub autoplay_loading: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueInsertTracksMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version_ref: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(fixed32, repeated, tag = "3")]
    pub track_ids: Vec<u32>,
    #[prost(int32, optional, tag = "4")]
    pub insert_after: Option<i32>,
    #[prost(fixed32, optional, tag = "5")]
    pub shuffle_seed: Option<u32>,
    #[prost(bytes = "vec", optional, tag = "6")]
    pub context_uuid: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "7")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "8")]
    pub autoplay_loading: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueAddTracksMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version_ref: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(fixed32, repeated, tag = "3")]
    pub track_ids: Vec<u32>,
    #[prost(fixed32, optional, tag = "4")]
    pub shuffle_seed: Option<u32>,
    #[prost(bytes = "vec", optional, tag = "5")]
    pub context_uuid: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "6")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "7")]
    pub autoplay_loading: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueRemoveTracksMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version_ref: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(int32, repeated, packed = "false", tag = "3")]
    pub queue_item_ids: Vec<i32>,
    #[prost(bool, optional, tag = "4")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "5")]
    pub autoplay_loading: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueReorderTracksMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version_ref: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(int32, repeated, packed = "false", tag = "3")]
    pub queue_item_ids: Vec<i32>,
    #[prost(int32, optional, tag = "4")]
    pub insert_after: Option<i32>,
    #[prost(bool, optional, tag = "5")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "6")]
    pub autoplay_loading: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetShuffleModeMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version_ref: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "3")]
    pub shuffle_mode: Option<bool>,
    #[prost(fixed32, optional, tag = "4")]
    pub shuffle_seed: Option<u32>,
    #[prost(int32, optional, tag = "5")]
    pub shuffle_pivot_queue_item_id: Option<i32>,
    #[prost(bool, optional, tag = "6")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "7")]
    pub autoplay_loading: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetAutoplayModeMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version_ref: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "3")]
    pub autoplay_mode: Option<bool>,
    #[prost(bool, optional, tag = "4")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "5")]
    pub autoplay_loading: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AutoplayLoadTracksMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version_ref: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(fixed32, repeated, tag = "3")]
    pub track_ids: Vec<u32>,
    #[prost(bytes = "vec", optional, tag = "4")]
    pub context_uuid: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AutoplayRemoveTracksMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version_ref: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(int32, repeated, packed = "false", tag = "3")]
    pub queue_item_ids: Vec<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetQueueStateMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version_ref: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(message, repeated, tag = "3")]
    pub tracks: Vec<SetQueueTrackWithContext>,
    #[prost(bool, optional, tag = "4")]
    pub shuffle_mode: Option<bool>,
    #[prost(int32, repeated, packed = "false", tag = "5")]
    pub shuffled_track_indexes: Vec<i32>,
    #[prost(bool, optional, tag = "6")]
    pub autoplay_mode: Option<bool>,
    #[prost(bool, optional, tag = "7")]
    pub autoplay_loading: Option<bool>,
    #[prost(message, repeated, tag = "8")]
    pub autoplay_tracks: Vec<SetQueueTrackWithContext>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AskForQueueStateMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version_ref: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueErrorMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(message, optional, tag = "3")]
    pub error: Option<ErrorMessage>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueClearedMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueStateMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(message, repeated, tag = "3")]
    pub tracks: Vec<QueueTrackWithContext>,
    #[prost(bool, optional, tag = "4")]
    pub shuffle_mode: Option<bool>,
    #[prost(int32, repeated, packed = "false", tag = "5")]
    pub shuffled_track_indexes: Vec<i32>,
    #[prost(bool, optional, tag = "6")]
    pub autoplay_mode: Option<bool>,
    #[prost(bool, optional, tag = "7")]
    pub autoplay_loading: Option<bool>,
    #[prost(message, repeated, tag = "8")]
    pub autoplay_tracks: Vec<QueueTrackWithContext>,
    #[prost(bytes = "vec", optional, tag = "100")]
    pub queue_hash: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueTracksLoadedMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(message, repeated, tag = "3")]
    pub tracks: Vec<QueueTrack>,
    #[prost(int32, optional, tag = "4")]
    pub queue_position: Option<i32>,
    #[prost(fixed32, optional, tag = "5")]
    pub shuffle_seed: Option<u32>,
    #[prost(int32, optional, tag = "6")]
    pub shuffle_pivot_queue_item_id: Option<i32>,
    #[prost(bool, optional, tag = "7")]
    pub shuffle_mode: Option<bool>,
    #[prost(bytes = "vec", optional, tag = "8")]
    pub context_uuid: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "9")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "10")]
    pub autoplay_loading: Option<bool>,
    #[prost(bytes = "vec", optional, tag = "100")]
    pub queue_hash: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueTracksInsertedMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(message, repeated, tag = "3")]
    pub tracks: Vec<QueueTrack>,
    #[prost(int32, optional, tag = "4")]
    pub insert_after: Option<i32>,
    #[prost(fixed32, optional, tag = "5")]
    pub shuffle_seed: Option<u32>,
    #[prost(bytes = "vec", optional, tag = "6")]
    pub context_uuid: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "7")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "8")]
    pub autoplay_loading: Option<bool>,
    #[prost(bytes = "vec", optional, tag = "100")]
    pub queue_hash: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueTracksAddedMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(message, repeated, tag = "3")]
    pub tracks: Vec<QueueTrack>,
    #[prost(fixed32, optional, tag = "4")]
    pub shuffle_seed: Option<u32>,
    #[prost(bytes = "vec", optional, tag = "5")]
    pub context_uuid: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "6")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "7")]
    pub autoplay_loading: Option<bool>,
    #[prost(bytes = "vec", optional, tag = "100")]
    pub queue_hash: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueTracksRemovedMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(int32, repeated, packed = "false", tag = "3")]
    pub queue_item_ids: Vec<i32>,
    #[prost(bool, optional, tag = "4")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "5")]
    pub autoplay_loading: Option<bool>,
    #[prost(bytes = "vec", optional, tag = "100")]
    pub queue_hash: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueTracksReorderedMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(int32, repeated, packed = "false", tag = "3")]
    pub queue_item_ids: Vec<i32>,
    #[prost(int32, optional, tag = "4")]
    pub insert_after: Option<i32>,
    #[prost(bool, optional, tag = "5")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "6")]
    pub autoplay_loading: Option<bool>,
    #[prost(bytes = "vec", optional, tag = "100")]
    pub queue_hash: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ShuffleModeSetMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "3")]
    pub shuffle_mode: Option<bool>,
    #[prost(fixed32, optional, tag = "4")]
    pub shuffle_seed: Option<u32>,
    #[prost(int32, optional, tag = "5")]
    pub shuffle_pivot_queue_item_id: Option<i32>,
    #[prost(bool, optional, tag = "6")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "7")]
    pub autoplay_loading: Option<bool>,
    #[prost(bytes = "vec", optional, tag = "100")]
    pub queue_hash: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AutoplayModeSetMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "3")]
    pub autoplay_mode: Option<bool>,
    #[prost(bool, optional, tag = "4")]
    pub autoplay_reset: Option<bool>,
    #[prost(bool, optional, tag = "5")]
    pub autoplay_loading: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AutoplayTracksLoadedMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(message, repeated, tag = "3")]
    pub tracks: Vec<QueueTrack>,
    #[prost(bytes = "vec", optional, tag = "4")]
    pub context_uuid: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AutoplayTracksRemovedMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub action_uuid: Option<Vec<u8>>,
    #[prost(int32, repeated, packed = "false", tag = "3")]
    pub queue_item_ids: Vec<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueTracksAddedFromAutoplayMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(int32, repeated, packed = "false", tag = "2")]
    pub queue_item_ids: Vec<i32>,
    #[prost(bytes = "vec", optional, tag = "100")]
    pub queue_hash: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererSetStateMessage {
    #[prost(int32, optional, tag = "1")]
    pub playing_state: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub current_position: Option<i32>,
    #[prost(message, optional, tag = "3")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(message, optional, tag = "4")]
    pub current_track: Option<QueueTrackWithContext>,
    #[prost(message, optional, tag = "5")]
    pub next_track: Option<QueueTrackWithContext>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererSetVolumeMessage {
    #[prost(int32, optional, tag = "1")]
    pub volume: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub volume_delta: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererSetActiveMessage {
    #[prost(bool, optional, tag = "1")]
    pub active: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererSetMaxAudioQualityMessage {
    #[prost(int32, optional, tag = "1")]
    pub max_audio_quality: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererSetLoopModeMessage {
    #[prost(int32, optional, tag = "1")]
    pub loop_mode: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererSetShuffleModeMessage {
    #[prost(bool, optional, tag = "1")]
    pub shuffle_mode: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererMuteVolumeMessage {
    #[prost(bool, optional, tag = "1")]
    pub value: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PlaybackPositionMessage {
    #[prost(fixed64, optional, tag = "1")]
    pub timestamp: Option<u64>,
    #[prost(int32, optional, tag = "2")]
    pub value: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererStateMessage {
    #[prost(int32, optional, tag = "1")]
    pub playing_state: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub buffer_state: Option<i32>,
    #[prost(message, optional, tag = "3")]
    pub current_position: Option<PlaybackPositionMessage>,
    #[prost(int32, optional, tag = "4")]
    pub duration: Option<i32>,
    #[prost(message, optional, tag = "5")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(int32, optional, tag = "6")]
    pub current_queue_item_id: Option<i32>,
    #[prost(int32, optional, tag = "7")]
    pub next_queue_item_id: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererStateUpdatedMessage {
    #[prost(message, optional, tag = "1")]
    pub state: Option<RendererStateMessage>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererVolumeChangedMessage {
    #[prost(int32, optional, tag = "1")]
    pub volume: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererVolumeMutedMessage {
    #[prost(bool, optional, tag = "1")]
    pub value: Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererFileAudioQualityChangedMessage {
    #[prost(int32, optional, tag = "1")]
    pub sampling_rate: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub bit_depth: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub nb_channels: Option<i32>,
    #[prost(int32, optional, tag = "4")]
    pub audio_quality: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererDeviceAudioQualityChangedMessage {
    #[prost(int32, optional, tag = "1")]
    pub sampling_rate: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub bit_depth: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub nb_channels: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendererMaxAudioQualityChangedMessage {
    #[prost(int32, optional, tag = "1")]
    pub max_audio_quality: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub network_type: Option<i32>,
}

// --- SRVR_CTRL session management messages (server → controller) ---

/// Type 81: Session state after joining
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CtrlSessionStateMessage {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub session_uuid: Option<Vec<u8>>,
    #[prost(int32, optional, tag = "2")]
    pub active_renderer_id: Option<i32>,
    #[prost(message, optional, tag = "3")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(int32, optional, tag = "4")]
    pub playing_state: Option<i32>,
    #[prost(int32, optional, tag = "5")]
    pub loop_mode: Option<i32>,
}

/// Nested player state for CtrlRendererStateUpdatedMessage
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CtrlRendererPlayerState {
    #[prost(int32, optional, tag = "1")]
    pub playing_state: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub buffer_state: Option<i32>,
    #[prost(message, optional, tag = "3")]
    pub current_position: Option<PlaybackPositionMessage>,
    #[prost(uint32, optional, tag = "4")]
    pub duration: Option<u32>,
    #[prost(int32, optional, tag = "5")]
    pub current_queue_item_id: Option<i32>,
}

/// Type 82: Renderer state updated (controller view)
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CtrlRendererStateUpdatedMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub status: Option<i32>,
    #[prost(message, optional, tag = "3")]
    pub player_state: Option<CtrlRendererPlayerState>,
}

/// Type 83: New renderer added to session
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CtrlAddRendererMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
    #[prost(message, optional, tag = "2")]
    pub device_info: Option<DeviceInfoMessage>,
}

/// Type 84: Renderer info updated
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CtrlUpdateRendererMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
    #[prost(message, optional, tag = "2")]
    pub device_info: Option<DeviceInfoMessage>,
}

/// Type 85: Renderer removed from session
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CtrlRemoveRendererMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
}

/// Type 86: Active renderer changed
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CtrlActiveRendererChangedMessage {
    #[prost(int32, optional, tag = "1")]
    pub active_renderer_id: Option<i32>,
}

/// Type 87: Volume changed on a renderer (controller view)
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CtrlVolumeChangedMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
    #[prost(uint32, optional, tag = "2")]
    pub volume: Option<u32>,
}

/// Type 97: Loop mode set
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CtrlLoopModeSetMessage {
    #[prost(int32, optional, tag = "1")]
    pub loop_mode: Option<i32>,
}

/// Type 98: Volume muted (controller view)
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CtrlVolumeMutedMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
    #[prost(bool, optional, tag = "2")]
    pub value: Option<bool>,
}

/// Type 99: Max audio quality changed (controller view)
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CtrlMaxAudioQualityChangedMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub max_audio_quality: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub network_type: Option<i32>,
}

/// Type 100: File audio quality changed (controller view)
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CtrlFileAudioQualityChangedMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
    #[prost(uint32, optional, tag = "2")]
    pub sampling_rate: Option<u32>,
    #[prost(uint32, optional, tag = "3")]
    pub bit_depth: Option<u32>,
    #[prost(uint32, optional, tag = "4")]
    pub nb_channels: Option<u32>,
    #[prost(int32, optional, tag = "5")]
    pub audio_quality: Option<i32>,
}

/// Type 101: Device audio quality changed (controller view)
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CtrlDeviceAudioQualityChangedMessage {
    #[prost(int32, optional, tag = "1")]
    pub renderer_id: Option<i32>,
    #[prost(uint32, optional, tag = "2")]
    pub sampling_rate: Option<u32>,
    #[prost(uint32, optional, tag = "3")]
    pub bit_depth: Option<u32>,
    #[prost(uint32, optional, tag = "4")]
    pub nb_channels: Option<u32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QConnectMessages {
    #[prost(fixed64, optional, tag = "1")]
    pub messages_time: Option<u64>,
    #[prost(int32, optional, tag = "2")]
    pub messages_id: Option<i32>,
    #[prost(message, repeated, tag = "3")]
    pub messages: Vec<QConnectMessage>,
}

/// Renderer per-track failure (unstreamable, geo-block, network, ...). Rides at
/// tag 3 of each `QConnectMessage` (`message_type = PLAYBACK_ERROR(2)`).
/// Shape: queue_version + queue_item_id + error_type (Common.ErrorType).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PlaybackErrorMessage {
    #[prost(message, optional, tag = "1")]
    pub queue_version: Option<QueueVersionRef>,
    #[prost(int32, optional, tag = "2")]
    pub queue_item_id: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub error_type: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QConnectMessage {
    #[prost(int32, optional, tag = "1")]
    pub message_type: Option<i32>,
    // Tag 3 of the official `Message` envelope record (message_type #1; error #2;
    // playback_error #3; oneof payloads 21-105). Resolved from the actual transport
    // bytes: the decoder receives the batch `QConnectMessages` and `playback_error`
    // rides inside each `messages[]` entry, NOT on the batch wrapper.
    #[prost(message, optional, tag = "3")]
    pub playback_error: Option<PlaybackErrorMessage>,
    #[prost(message, optional, tag = "21")]
    pub rndr_srvr_join_session: Option<JoinSessionMessage>,
    #[prost(message, optional, tag = "22")]
    pub rndr_srvr_device_info_updated: Option<RendererDeviceInfoUpdatedMessage>,
    #[prost(message, optional, tag = "23")]
    pub rndr_srvr_state_updated: Option<RendererStateUpdatedMessage>,
    #[prost(message, optional, tag = "25")]
    pub rndr_srvr_volume_changed: Option<RendererVolumeChangedMessage>,
    #[prost(message, optional, tag = "26")]
    pub rndr_srvr_file_audio_quality_changed: Option<RendererFileAudioQualityChangedMessage>,
    #[prost(message, optional, tag = "27")]
    pub rndr_srvr_device_audio_quality_changed:
        Option<RendererDeviceAudioQualityChangedMessage>,
    #[prost(message, optional, tag = "28")]
    pub rndr_srvr_max_audio_quality_changed: Option<RendererMaxAudioQualityChangedMessage>,
    #[prost(message, optional, tag = "29")]
    pub rndr_srvr_volume_muted: Option<RendererVolumeMutedMessage>,
    #[prost(message, optional, tag = "41")]
    pub srvr_rndr_set_state: Option<RendererSetStateMessage>,
    #[prost(message, optional, tag = "42")]
    pub srvr_rndr_set_volume: Option<RendererSetVolumeMessage>,
    #[prost(message, optional, tag = "43")]
    pub srvr_rndr_set_active: Option<RendererSetActiveMessage>,
    #[prost(message, optional, tag = "44")]
    pub srvr_rndr_set_max_audio_quality: Option<RendererSetMaxAudioQualityMessage>,
    #[prost(message, optional, tag = "45")]
    pub srvr_rndr_set_loop_mode: Option<RendererSetLoopModeMessage>,
    #[prost(message, optional, tag = "46")]
    pub srvr_rndr_set_shuffle_mode: Option<RendererSetShuffleModeMessage>,
    #[prost(message, optional, tag = "47")]
    pub srvr_rndr_mute_volume: Option<RendererMuteVolumeMessage>,
    #[prost(message, optional, tag = "61")]
    pub ctrl_srvr_join_session: Option<JoinSessionMessage>,
    #[prost(message, optional, tag = "62")]
    pub ctrl_srvr_set_player_state: Option<SetPlayerStateMessage>,
    #[prost(message, optional, tag = "63")]
    pub ctrl_srvr_set_active_renderer: Option<SetActiveRendererMessage>,
    #[prost(message, optional, tag = "64")]
    pub ctrl_srvr_set_volume: Option<SetVolumeMessage>,
    #[prost(message, optional, tag = "65")]
    pub ctrl_srvr_clear_queue: Option<ClearQueueMessage>,
    #[prost(message, optional, tag = "66")]
    pub ctrl_srvr_queue_load_tracks: Option<QueueLoadTracksMessage>,
    #[prost(message, optional, tag = "67")]
    pub ctrl_srvr_queue_insert_tracks: Option<QueueInsertTracksMessage>,
    #[prost(message, optional, tag = "68")]
    pub ctrl_srvr_queue_add_tracks: Option<QueueAddTracksMessage>,
    #[prost(message, optional, tag = "69")]
    pub ctrl_srvr_queue_remove_tracks: Option<QueueRemoveTracksMessage>,
    #[prost(message, optional, tag = "70")]
    pub ctrl_srvr_queue_reorder_tracks: Option<QueueReorderTracksMessage>,
    #[prost(message, optional, tag = "71")]
    pub ctrl_srvr_set_shuffle_mode: Option<SetShuffleModeMessage>,
    #[prost(message, optional, tag = "72")]
    pub ctrl_srvr_set_loop_mode: Option<SetLoopModeMessage>,
    #[prost(message, optional, tag = "73")]
    pub ctrl_srvr_mute_volume: Option<MuteVolumeMessage>,
    #[prost(message, optional, tag = "74")]
    pub ctrl_srvr_set_max_audio_quality: Option<SetMaxAudioQualityMessage>,
    #[prost(message, optional, tag = "75")]
    pub ctrl_srvr_set_queue_state: Option<SetQueueStateMessage>,
    #[prost(message, optional, tag = "76")]
    pub ctrl_srvr_ask_for_queue_state: Option<AskForQueueStateMessage>,
    #[prost(message, optional, tag = "77")]
    pub ctrl_srvr_ask_for_renderer_state: Option<AskForRendererStateMessage>,
    #[prost(message, optional, tag = "78")]
    pub ctrl_srvr_set_autoplay_mode: Option<SetAutoplayModeMessage>,
    #[prost(message, optional, tag = "79")]
    pub ctrl_srvr_autoplay_load_tracks: Option<AutoplayLoadTracksMessage>,
    #[prost(message, optional, tag = "80")]
    pub ctrl_srvr_autoplay_remove_tracks: Option<AutoplayRemoveTracksMessage>,
    #[prost(message, optional, tag = "81")]
    pub srvr_ctrl_session_state: Option<CtrlSessionStateMessage>,
    #[prost(message, optional, tag = "82")]
    pub srvr_ctrl_renderer_state_updated: Option<CtrlRendererStateUpdatedMessage>,
    #[prost(message, optional, tag = "83")]
    pub srvr_ctrl_add_renderer: Option<CtrlAddRendererMessage>,
    #[prost(message, optional, tag = "84")]
    pub srvr_ctrl_update_renderer: Option<CtrlUpdateRendererMessage>,
    #[prost(message, optional, tag = "85")]
    pub srvr_ctrl_remove_renderer: Option<CtrlRemoveRendererMessage>,
    #[prost(message, optional, tag = "86")]
    pub srvr_ctrl_active_renderer_changed: Option<CtrlActiveRendererChangedMessage>,
    #[prost(message, optional, tag = "87")]
    pub srvr_ctrl_volume_changed: Option<CtrlVolumeChangedMessage>,
    #[prost(message, optional, tag = "88")]
    pub srvr_ctrl_queue_error_message: Option<QueueErrorMessage>,
    #[prost(message, optional, tag = "89")]
    pub srvr_ctrl_queue_cleared: Option<QueueClearedMessage>,
    #[prost(message, optional, tag = "90")]
    pub srvr_ctrl_queue_state: Option<QueueStateMessage>,
    #[prost(message, optional, tag = "91")]
    pub srvr_ctrl_queue_tracks_loaded: Option<QueueTracksLoadedMessage>,
    #[prost(message, optional, tag = "92")]
    pub srvr_ctrl_queue_tracks_inserted: Option<QueueTracksInsertedMessage>,
    #[prost(message, optional, tag = "93")]
    pub srvr_ctrl_queue_tracks_added: Option<QueueTracksAddedMessage>,
    #[prost(message, optional, tag = "94")]
    pub srvr_ctrl_queue_tracks_removed: Option<QueueTracksRemovedMessage>,
    #[prost(message, optional, tag = "95")]
    pub srvr_ctrl_queue_tracks_reordered: Option<QueueTracksReorderedMessage>,
    #[prost(message, optional, tag = "96")]
    pub srvr_ctrl_shuffle_mode_set: Option<ShuffleModeSetMessage>,
    #[prost(message, optional, tag = "97")]
    pub srvr_ctrl_loop_mode_set: Option<CtrlLoopModeSetMessage>,
    #[prost(message, optional, tag = "98")]
    pub srvr_ctrl_volume_muted: Option<CtrlVolumeMutedMessage>,
    #[prost(message, optional, tag = "99")]
    pub srvr_ctrl_max_audio_quality_changed: Option<CtrlMaxAudioQualityChangedMessage>,
    #[prost(message, optional, tag = "100")]
    pub srvr_ctrl_file_audio_quality_changed: Option<CtrlFileAudioQualityChangedMessage>,
    #[prost(message, optional, tag = "101")]
    pub srvr_ctrl_device_audio_quality_changed: Option<CtrlDeviceAudioQualityChangedMessage>,
    #[prost(message, optional, tag = "102")]
    pub srvr_ctrl_autoplay_mode_set: Option<AutoplayModeSetMessage>,
    #[prost(message, optional, tag = "103")]
    pub srvr_ctrl_autoplay_tracks_loaded: Option<AutoplayTracksLoadedMessage>,
    #[prost(message, optional, tag = "104")]
    pub srvr_ctrl_autoplay_tracks_removed: Option<AutoplayTracksRemovedMessage>,
    #[prost(message, optional, tag = "105")]
    pub srvr_ctrl_queue_tracks_added_from_autoplay: Option<QueueTracksAddedFromAutoplayMessage>,
}
