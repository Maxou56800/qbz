//! QBZ Audio - Audio backend system for bit-perfect playback
//!
//! This crate provides the audio backend abstraction layer:
//! - Backend trait and implementations (PipeWire, ALSA, PulseAudio)
//! - Audio device enumeration and selection
//! - Loudness analysis and normalization
//! - Diagnostic tools
//!
//! # CRITICAL: This code is IMMUTABLE
//!
//! The audio backend system was carefully designed for bit-perfect playback.
//! Do NOT modify the logic in these files without understanding the full
//! architecture. See `qbz-nix-docs/AUDIO_BACKENDS.md` for details.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                     qbz-audio (Tier 1)                      │
//! │  Audio backends, device management, loudness analysis       │
//! └─────────────────────────────────────────────────────────────┘
//!                              ↑
//!                      ┌───────┴───────┐
//!                      │  qbz-models   │
//!                      │   (Tier 0)    │
//!                      └───────────────┘
//! ```

#[cfg(target_os = "linux")]
pub mod alsa_backend;
pub mod alsa_direct;
#[cfg(target_os = "linux")]
mod pcm_write;
pub mod alsa_hardware_volume;
pub mod wasapi_direct;
/// Endpoint capabilities: the exclusive-mode rate sweep and the hotplug watch.
#[cfg(windows)]
pub mod wasapi_backend;
#[cfg(target_os = "linux")]
pub mod alsa_error_handler;
pub mod analysis;
pub mod analyzer_tap;
pub mod backend;
pub mod coreaudio_direct;
pub mod dac_capabilities;
pub mod dac_probe;
pub mod device_filter;
pub mod device_reservation;
pub mod diagnostic;
pub mod dynamic_amplify;
pub mod health;
#[cfg(target_os = "linux")]
pub mod jack_backend;
pub mod loudness;
pub mod loudness_analyzer;
pub mod loudness_cache;
pub mod loudness_scan;
pub mod network_throttle;
pub mod output_sinks;
#[cfg(target_os = "linux")]
pub mod pipewire_backend;
#[cfg(target_os = "linux")]
pub mod pulse_backend;
pub mod seek_waveform;
pub mod settings;
pub mod stream_health;
pub mod visualizer;

// Re-export commonly used types
#[cfg(target_os = "linux")]
pub use alsa_backend::{
    device_supports_sample_rate, get_device_supported_rates, normalize_device_id_to_stable,
    resolve_stable_to_current_hw,
};
pub use alsa_direct::AlsaDirectStream;
pub use wasapi_direct::{Rung, LADDER};
pub use analysis::SpectralAnalyzer;
pub use analyzer_tap::{AnalyzerMessage, AnalyzerTap, AnalyzerWaveformTrack};
pub use backend::{
    AlsaDirectError, AlsaPlugin, AudioBackend, AudioBackendType, AudioDevice, BackendConfig,
    BackendManager, BackendResult, BitPerfectMode,
};
pub use coreaudio_direct::CoreAudioExclusiveGuard;
pub use dac_capabilities::{query_dac_capabilities, DacCapabilities};
pub use dac_probe::{negotiated_active_rate, negotiated_stream_rate, NegotiatedRate};
pub use device_reservation::{DeviceReservation, ReservationError};
pub use diagnostic::{AudioDiagnostic, BitDepthResult, DiagnosticSource};
pub use dynamic_amplify::DynamicAmplify;
pub use health::{
    audio_stack_health, detect_distro, detect_init, detect_sandbox, AudioStackHealth, Distro,
    InitSystem, Sandbox,
};
#[cfg(target_os = "linux")]
pub use jack_backend::JackStream;
pub use loudness::{calculate_gain_factor, db_to_linear, extract_replaygain, ReplayGainData};
pub use loudness_analyzer::LoudnessAnalyzer;
pub use loudness_cache::{gain_for, lufs_from_replaygain, LoudnessCache, LoudnessSource, StoredLoudness};
pub use loudness_scan::{measure as measure_loudness, TrackLoudness};
pub use output_sinks::{list_output_sinks, OutputSinkInfo};
pub use seek_waveform::{
    register_seek_waveform_key, seek_waveform_content_key, seek_waveform_snapshot,
    set_seek_waveform_enabled, SeekWaveformSnapshot, SEEK_WAVEFORM_BINS,
};
pub use settings::AudioSettings;
pub use visualizer::{RingBuffer, TappedSource, VisualizerTap};

/// Stub: returns the ID unchanged on non-Linux (no ALSA normalization needed).
#[cfg(not(target_os = "linux"))]
pub fn normalize_device_id_to_stable(id: &str) -> String {
    id.to_string()
}

/// Stub: no ALSA device resolution on non-Linux.
#[cfg(not(target_os = "linux"))]
pub fn resolve_stable_to_current_hw(_stable: &str) -> Option<String> {
    None
}

/// Stub: no ALSA sample rate probing on non-Linux.
#[cfg(not(target_os = "linux"))]
pub fn device_supports_sample_rate(_device_id: &str, _sample_rate: u32) -> Option<bool> {
    None
}

/// Stub: no ALSA rate enumeration on non-Linux.
#[cfg(not(target_os = "linux"))]
pub fn get_device_supported_rates(_device_id: &str) -> Option<Vec<u32>> {
    None
}

#[cfg(target_os = "linux")]
mod pcm_sample;
