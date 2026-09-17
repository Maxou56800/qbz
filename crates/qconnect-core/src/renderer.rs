use serde::{Deserialize, Serialize};

use crate::QueueItem;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QConnectRendererState {
    pub active: Option<bool>,
    pub playing_state: Option<i32>,
    pub current_position_ms: Option<u64>,
    pub current_track: Option<QueueItem>,
    pub next_track: Option<QueueItem>,
    pub volume: Option<i32>,
    pub volume_delta: Option<i32>,
    pub muted: Option<bool>,
    pub max_audio_quality: Option<i32>,
    pub loop_mode: Option<i32>,
    pub shuffle_mode: Option<bool>,
    pub updated_at_ms: u64,
}

impl Default for QConnectRendererState {
    fn default() -> Self {
        Self {
            active: None,
            playing_state: None,
            current_position_ms: None,
            current_track: None,
            next_track: None,
            volume: None,
            volume_delta: None,
            muted: None,
            max_audio_quality: None,
            loop_mode: None,
            shuffle_mode: None,
            updated_at_ms: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RendererCommand {
    SetState {
        playing_state: Option<i32>,
        current_position_ms: Option<u64>,
        current_track: Option<QueueItem>,
        next_track: Option<QueueItem>,
    },
    SetVolume {
        volume: Option<i32>,
        volume_delta: Option<i32>,
    },
    SetActive {
        active: bool,
    },
    SetMaxAudioQuality {
        max_audio_quality: i32,
    },
    SetLoopMode {
        loop_mode: i32,
    },
    SetShuffleMode {
        shuffle_mode: bool,
    },
    MuteVolume {
        value: bool,
    },
}

pub fn apply_renderer_command(
    state: &mut QConnectRendererState,
    command: &RendererCommand,
    now_ms: u64,
) {
    match command {
        RendererCommand::SetState {
            playing_state,
            current_position_ms,
            current_track,
            next_track,
        } => {
            // Position belongs to an occurrence, never to the next track.
            if current_track.as_ref().zip(state.current_track.as_ref()).is_some_and(|(next, previous)|
                next.track_id != previous.track_id || next.queue_item_id != previous.queue_item_id) {
                state.current_position_ms = None;
                state.next_track = None;
            }
            // Only overwrite fields when the server provides a value.
            // Server pause/resume commands send None for position and tracks,
            // but the renderer should retain the last known values so that
            // subsequent renderer reports include them correctly.
            if playing_state.is_some() {
                state.playing_state = *playing_state;
            }
            if current_position_ms.is_some() {
                state.current_position_ms = *current_position_ms;
            }
            if current_track.is_some() {
                state.current_track = current_track.clone();
            }
            if next_track.is_some() {
                state.next_track = next_track.clone();
            }
        }
        RendererCommand::SetVolume {
            volume,
            volume_delta,
        } => {
            if let Some(volume) = volume {
                state.volume = Some(*volume);
            }
            if let Some(volume_delta) = volume_delta {
                state.volume_delta = Some(*volume_delta);
                if let Some(current) = state.volume {
                    let next = current.saturating_add(*volume_delta).clamp(0, 100);
                    state.volume = Some(next);
                }
            }
        }
        RendererCommand::SetActive { active } => {
            state.active = Some(*active);
        }
        RendererCommand::SetMaxAudioQuality { max_audio_quality } => {
            state.max_audio_quality = Some(*max_audio_quality);
        }
        RendererCommand::SetLoopMode { loop_mode } => {
            state.loop_mode = Some(*loop_mode);
        }
        RendererCommand::SetShuffleMode { shuffle_mode } => {
            state.shuffle_mode = Some(*shuffle_mode);
        }
        RendererCommand::MuteVolume { value } => {
            state.muted = Some(*value);
        }
    }

    state.updated_at_ms = now_ms;
}

#[cfg(test)]
mod tests {
    use crate::{apply_renderer_command, QConnectRendererState, QueueItem, RendererCommand};

    #[test]
    fn partial_first_target_keeps_position_but_a_new_occurrence_does_not() {
        let mut state = QConnectRendererState::default();
        let item = QueueItem { track_context_uuid: String::new(), track_id: 10, queue_item_id: 0 };
        let command = |track, position| RendererCommand::SetState {
            playing_state: Some(3), current_position_ms: position,
            current_track: track, next_track: None,
        };
        apply_renderer_command(&mut state, &command(None, Some(45_000)), 1);
        apply_renderer_command(&mut state, &command(Some(item.clone()), None), 2);
        assert_eq!(state.current_position_ms, Some(45_000));
        let mut revised_context = item.clone();
        revised_context.track_context_uuid = "context".into();
        apply_renderer_command(&mut state, &command(Some(revised_context), None), 3);
        assert_eq!(state.current_position_ms, Some(45_000));
        let mut repeated = item;
        repeated.queue_item_id = 2;
        apply_renderer_command(&mut state, &command(Some(repeated), None), 4);
        assert_eq!(state.current_position_ms, None);
    }

    #[test]
    fn set_state_replaces_renderer_cursor() {
        let mut state = QConnectRendererState::default();
        let current_track = QueueItem {
            track_context_uuid: "ctx-a".to_string(),
            track_id: 11,
            queue_item_id: 101,
        };
        let next_track = QueueItem {
            track_context_uuid: "ctx-a".to_string(),
            track_id: 12,
            queue_item_id: 102,
        };

        apply_renderer_command(
            &mut state,
            &RendererCommand::SetState {
                playing_state: Some(2),
                current_position_ms: Some(42_123),
                current_track: Some(current_track.clone()),
                next_track: Some(next_track.clone()),
            },
            900,
        );

        assert_eq!(state.playing_state, Some(2));
        assert_eq!(state.current_position_ms, Some(42_123));
        assert_eq!(state.current_track, Some(current_track));
        assert_eq!(state.next_track, Some(next_track));
        assert_eq!(state.updated_at_ms, 900);
    }

    #[test]
    fn volume_delta_updates_current_volume_when_known() {
        let mut state = QConnectRendererState {
            volume: Some(45),
            ..Default::default()
        };

        apply_renderer_command(
            &mut state,
            &RendererCommand::SetVolume {
                volume: None,
                volume_delta: Some(8),
            },
            1000,
        );
        assert_eq!(state.volume, Some(53));
        assert_eq!(state.volume_delta, Some(8));
        assert_eq!(state.updated_at_ms, 1000);
    }

    #[test]
    fn set_state_none_does_not_overwrite_existing_values() {
        let current_track = QueueItem {
            track_context_uuid: "ctx-a".to_string(),
            track_id: 11,
            queue_item_id: 101,
        };
        let mut state = QConnectRendererState {
            playing_state: Some(2),
            current_position_ms: Some(30_000),
            current_track: Some(current_track.clone()),
            ..Default::default()
        };

        // Simulate a server pause command that sends None for position/tracks
        apply_renderer_command(
            &mut state,
            &RendererCommand::SetState {
                playing_state: Some(3),
                current_position_ms: None,
                current_track: None,
                next_track: None,
            },
            1500,
        );

        assert_eq!(state.playing_state, Some(3)); // updated
        assert_eq!(state.current_position_ms, Some(30_000)); // retained
        assert_eq!(state.current_track, Some(current_track)); // retained
    }
}
