// crates/qbzd/src/api/status.rs — `GET /api/status` composite contract
// (02-cli-and-api.md §3.3.3; memo D14). Struct shape only in T2 — the
// `auth`/`qconnect`/`last_errors` sections already read from `DaemonShared`;
// `audio`/`playback`/`network` are placeholders wired by T3 (audio/playback
// driver), T6 (HTTP server + network reachability) and T10 (qconnect report
// tick, already flowing through `DaemonShared::qconnect`).
use std::io::Cursor;

use serde::Serialize;
use tiny_http::Response;

use crate::state::{AuthState, LatchedErrors, QconnectStatus};

#[derive(Debug, Clone, Serialize)]
pub struct StatusDoc {
    pub version: String,
    pub api_version: u32,
    pub uptime_secs: u64,
    pub data_root: String,
    pub driver_tick_age_ms: Option<u64>,
    pub auth: AuthStatus,
    pub audio: AudioStatus,
    pub playback: PlaybackStatus,
    pub qconnect: QconnectStatus,
    pub network: NetworkStatus,
    pub last_errors: LatchedErrors,
    pub playback_cache: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthStatus {
    pub state: AuthState,
    pub user_id: Option<u64>,
    pub subscription: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioStatus {
    pub backend: Option<String>,
    pub configured_device: Option<String>,
    pub device_present: bool,
    pub device_open: bool,
    /// `BitPerfectMode` serde variants: "DirectHardware"|"PluginFallback"|"Disabled"
    /// (crates/qbz-audio/src/backend.rs:226-233). Kept as a plain string here so
    /// this crate does not need to depend on the exact qbz-audio enum shape yet.
    pub bit_perfect: Option<String>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub hardware_volume_enabled: bool,
    pub hardware_volume_active: bool,
    pub hardware_volume_control: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaybackStatus {
    pub state: String,
    pub track_id: Option<u64>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub position: Option<u64>,
    pub duration: Option<u64>,
    pub volume: f32,
    pub muted: bool,
    pub queue_len: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkStatus {
    pub online: bool,
}

// ============================ live handlers (T6) ============================

/// `GET /api/info` (02 §3.3.2) — identity for the CLI's version-skew diagnosis
/// (§1.6, the single sanctioned second request). Deliberately minimal.
pub fn info(state: &super::ApiState) -> Response<Cursor<Vec<u8>>> {
    let uptime = state
        .shared
        .lock()
        .ok()
        .map(|s| s.started_at.elapsed().as_secs())
        .unwrap_or(0);
    super::json(
        200,
        serde_json::json!({
            "app": "qbzd",
            "version": env!("CARGO_PKG_VERSION"),
            "api_version": crate::API_VERSION,
            "bind": state.bind,
            "uptime_secs": uptime,
            "data_root": state.roots.data.display().to_string(),
        }),
    )
}

/// `GET /api/status` (02 §3.3.3) — the composite daemon status. ALWAYS 200;
/// the CLI maps degradation (needs_auth, missing device) to exit codes.
pub fn status(state: &super::ApiState) -> Response<Cursor<Vec<u8>>> {
    let doc = assemble_live(state);
    let mut value = serde_json::to_value(&doc).unwrap_or_else(|_| serde_json::json!({}));
    // `StatusDoc.playback.volume` is f32; plain `to_value` widens it via
    // `Number::from_f32` (f32→f64, `0.8` → `0.800000011920929`). Overwrite
    // with the canonical form — see `super::canon_volume`.
    if let Some(vol) = value.pointer_mut("/playback/volume") {
        *vol = super::canon_volume(doc.playback.volume);
    }
    super::json(200, value)
}

/// Compose [`StatusDoc`] from live sources: `DaemonShared` (auth/qconnect/latched
/// errors/tick age), the Player's sync getters via `get_playback_event`, the
/// queue via an async core call (`block_on` on the daemon runtime — this is a
/// plain serving thread, never a tokio worker, so no panic), and the audio
/// store + TTL device cache for the audio block.
fn assemble_live(state: &super::ApiState) -> StatusDoc {
    // 1. snapshot DaemonShared, then DROP the guard before any block_on so the
    //    mutex is never held across an await point.
    let (auth, user_id, subscription, last_errors, qconnect, tick_age, muted, uptime, network_online) =
        match state.shared.lock() {
            Ok(s) => (
                s.auth,
                s.user_id,
                s.subscription.clone(),
                s.last_errors.clone(),
                s.qconnect.clone(),
                s.driver_last_tick.map(|t| t.elapsed().as_millis() as u64),
                s.muted,
                s.started_at.elapsed().as_secs(),
                s.network_online(),
            ),
            Err(_) => (
                AuthState::Restoring,
                None,
                None,
                LatchedErrors::default(),
                QconnectStatus::default(),
                None,
                false,
                0,
                true,
            ),
        };

    // 2. live player snapshot (all sync atomics, folded into one PlaybackEvent).
    let player = state.runtime.core().player();
    let ev = player.get_playback_event();
    let device_open = player.state.current_device().is_some();

    // 3. queue — async core read, driven from this non-worker thread.
    let queue = state.rt.block_on(state.runtime.core().get_queue_state());

    // 4. audio config from the store; device_present from the TTL cache. An OPEN
    //    device counts as present: CPAL enumerates DESCRIPTIONS ("HiFiBerry DAC+
    //    ..."), never `hw:CARD=...` ids, so a playing direct-hw stream would
    //    otherwise report `not present` (false negative).
    let settings = state.audio.get_settings().ok();
    let backend = settings.as_ref().and_then(|s| backend_label(s.backend_type));
    let configured_device = settings.as_ref().and_then(|s| s.output_device.clone());
    let hardware_volume_enabled = settings
        .as_ref()
        .is_some_and(|settings| settings.alsa_hardware_volume);
    let hardware_volume_control = settings
        .as_ref()
        .and_then(|settings| settings.selected_alsa_hardware_volume_control())
        .map(|control| control.to_string());
    let device_present = match &configured_device {
        None => true, // system default is always "present"
        Some(dev) => device_open || device_is_present(state, dev),
    };

    // 5. playback block. `stopped` when nothing is loaded and the queue has no
    //    current track; otherwise `playing`/`paused`.
    let has_track = queue.current_track.is_some();
    let pstate = if ev.is_playing {
        "playing"
    } else if has_track || player.has_loaded_audio() {
        "paused"
    } else {
        "stopped"
    };
    let stopped = pstate == "stopped";
    let (title, artist, track_id) = match &queue.current_track {
        Some(t) => (Some(t.title.clone()), Some(t.artist.clone()), Some(t.id)),
        None if ev.track_id != 0 => (None, None, Some(ev.track_id)),
        None => (None, None, None),
    };

    StatusDoc {
        playback_cache: serde_json::to_value(player.playback_cache_stats()).ok(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        api_version: crate::API_VERSION,
        uptime_secs: uptime,
        data_root: state.roots.data.display().to_string(),
        driver_tick_age_ms: tick_age,
        auth: AuthStatus {
            state: auth,
            user_id,
            subscription,
        },
        audio: AudioStatus {
            backend,
            configured_device,
            device_present,
            device_open,
            bit_perfect: bitperfect_label(ev.bit_perfect_mode),
            sample_rate: ev.sample_rate,
            bit_depth: ev.bit_depth,
            hardware_volume_enabled,
            hardware_volume_active: ev.hardware_volume_active,
            hardware_volume_control,
        },
        playback: PlaybackStatus {
            state: pstate.to_string(),
            track_id: if stopped { None } else { track_id },
            title: if stopped { None } else { title },
            artist: if stopped { None } else { artist },
            position: if stopped { None } else { Some(ev.position) },
            duration: if stopped { None } else { Some(ev.duration) },
            volume: ev.volume,
            muted,
            queue_len: queue.total_tracks,
        },
        qconnect,
        network: NetworkStatus { online: network_online },
        last_errors,
    }
}

/// `BitPerfectMode` → its serde variant string (02 §3.3.3:
/// `"DirectHardware"|"PluginFallback"|"Disabled"`). `None` = no active stream.
fn bitperfect_label(m: Option<qbz_audio::BitPerfectMode>) -> Option<String> {
    use qbz_audio::BitPerfectMode as M;
    m.map(|m| {
        match m {
            M::DirectHardware => "DirectHardware",
            M::PluginFallback => "PluginFallback",
            M::Disabled => "Disabled",
        }
        .to_string()
    })
}

/// Configured backend → the lowercase label the status block shows. `None`
/// (auto-detect) stays `null` until a stream picks a concrete backend.
fn backend_label(b: Option<qbz_audio::AudioBackendType>) -> Option<String> {
    use qbz_audio::AudioBackendType as B;
    b.map(|b| {
        match b {
            B::PipeWire => "pipewire",
            B::Alsa => "alsa",
            B::Pulse => "pulse",
            B::Jack => "jack",
            B::WasapiExclusive => "wasapi",
            B::SystemDefault => "system",
        }
        .to_string()
    })
}

/// Best-effort presence check against the TTL-cached device enumeration. Exact
/// device identity is refined in T10; here a substring match on either side
/// tolerates the CPAL-name vs `hw:` mismatch without false negatives on a match.
fn device_is_present(state: &super::ApiState, dev: &str) -> bool {
    cached_device_names(state)
        .iter()
        .any(|n| n == dev || n.contains(dev) || dev.contains(n.as_str()))
}

/// Device names, re-enumerated at most every 5 s (a `status` poll must not
/// re-scan CPAL on every call). On enumeration failure the timestamp is still
/// bumped so a broken audio stack is not hammered.
fn cached_device_names(state: &super::ApiState) -> Vec<String> {
    use std::time::{Duration, Instant};
    let mut cache = match state.devices.lock() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let fresh = cache
        .at
        .map(|t| t.elapsed() < Duration::from_secs(5))
        .unwrap_or(false);
    if !fresh {
        if let Ok(sinks) = qbz_audio::output_sinks::list_output_sinks() {
            cache.names = sinks.into_iter().map(|s| s.name).collect();
        }
        cache.at = Some(Instant::now());
    }
    cache.names.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_exclusive_backend_has_a_stable_status_label() {
        assert_eq!(
            backend_label(Some(qbz_audio::AudioBackendType::WasapiExclusive)).as_deref(),
            Some("wasapi")
        );
    }

    /// A NeedsAuth [`StatusDoc`] built directly (no live runtime), matching the
    /// 02 §3.3.3 NeedsAuth fragment — the serde-shape contract these tests pin.
    fn needs_auth_doc() -> StatusDoc {
        StatusDoc {
            playback_cache: None,            version: "2.1.0".into(),
            api_version: crate::API_VERSION,
            uptime_secs: 261_360,
            data_root: "/home/pi/.local/share/qbzd".into(),
            driver_tick_age_ms: Some(210),
            auth: AuthStatus {
                state: AuthState::NeedsAuth,
                user_id: None,
                subscription: None,
            },
            audio: AudioStatus {
                backend: Some("alsa".into()),
                configured_device: None,
                device_present: true,
                device_open: false,
                bit_perfect: None,
                sample_rate: None,
                bit_depth: None,
                hardware_volume_enabled: false,
                hardware_volume_active: false,
                hardware_volume_control: None,
            },
            playback: PlaybackStatus {
                state: "stopped".into(),
                track_id: None,
                title: None,
                artist: None,
                position: None,
                duration: None,
                volume: 0.0,
                muted: false,
                queue_len: 0,
            },
            qconnect: QconnectStatus::default(),
            network: NetworkStatus { online: true },
            last_errors: LatchedErrors {
                stream: None,
                auth: Some("token rejected by Qobuz (401) — cleared".into()),
                transport: None,
            },
        }
    }

    #[test]
    fn status_doc_mirrors_the_top_level_contract_keys() {
        // 02-cli-and-api.md §3.3.3 top-level keys, exactly.
        let json = serde_json::to_value(needs_auth_doc()).unwrap();
        let obj = json.as_object().unwrap();
        for key in [
            "version",
            "api_version",
            "uptime_secs",
            "data_root",
            "driver_tick_age_ms",
            "auth",
            "audio",
            "playback",
            "qconnect",
            "network",
            "last_errors",
        ] {
            assert!(obj.contains_key(key), "missing top-level key: {key}");
        }
    }

    #[test]
    fn needs_auth_fragment_matches_spec_example() {
        // 02 §3.3.3 NeedsAuth example fragment + auth.state serde string.
        let doc = needs_auth_doc();
        assert_eq!(doc.auth.state, AuthState::NeedsAuth);
        assert!(doc.auth.user_id.is_none());
        assert!(doc.auth.subscription.is_none());
        assert_eq!(
            doc.last_errors.auth.as_deref(),
            Some("token rejected by Qobuz (401) — cleared")
        );
        let json = serde_json::to_value(&doc.auth).unwrap();
        assert_eq!(json["state"], "needs_auth");
    }

    #[test]
    fn status_doc_playback_volume_serializes_canonically() {
        // Pins the `status()` pointer-overwrite: `to_value(&doc)` widens the
        // f32 `playback.volume` via `Number::from_f32`; the fix must land
        // `0.8` on the wire, never `0.800000011920929`.
        let mut doc = needs_auth_doc();
        doc.playback.volume = 0.8f32;
        let mut value = serde_json::to_value(&doc).unwrap();
        if let Some(vol) = value.pointer_mut("/playback/volume") {
            *vol = crate::api::canon_volume(doc.playback.volume);
        }
        let rendered = serde_json::to_string(&value).unwrap();
        assert!(rendered.contains("\"volume\":0.8"), "got: {rendered}");
        assert!(!rendered.contains("0.80000"), "got: {rendered}");
    }

    #[test]
    fn audio_block_serializes_the_documented_keys() {
        // 02 §3.3.3 audio object — the shape the live assembler fills.
        let json = serde_json::to_value(needs_auth_doc()).unwrap();
        let audio = json["audio"].as_object().unwrap();
        for key in [
            "backend",
            "configured_device",
            "device_present",
            "device_open",
            "bit_perfect",
            "sample_rate",
            "bit_depth",
        ] {
            assert!(audio.contains_key(key), "missing audio key: {key}");
        }
    }
}
