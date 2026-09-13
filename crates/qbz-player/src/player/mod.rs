//! Audio player module
//!
//! Handles audio playback with support for:
//! - HTTP streaming from Qobuz
//! - FLAC, MP3 decoding via symphonia
//! - Gapless playback
//! - Volume control
//! - Real-time position tracking via events
//!
//! Uses a dedicated audio thread since rodio's OutputStream is not Send.
//! Supports both rodio (PipeWire/Pulse) and direct ALSA (hw: devices).

mod playback_engine;
mod streaming_source;
mod disk_playback;
mod loudness_scan;
mod normalization;

pub mod memory_tuning;

pub use memory_tuning::{
    apply_memory_tuning, audio_cache_l1_max_bytes, is_low_memory_class, oversized_for_l1,
};
pub use streaming_source::{
    max_initial_buffer_bytes, set_max_initial_buffer_bytes, BufferWriter, BufferedMediaSource,
    InMemorySource, IncrementalStreamingSource, StreamingConfig,
};

use rodio::buffer::SamplesBuffer;
use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::cpal::{
    BufferSize, SampleFormat, StreamConfig, SupportedBufferSize, SupportedStreamConfig,
};
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Source};
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CodecType, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::default::{get_codecs, get_probe};

use playback_engine::PlaybackEngine;
use qbz_audio::{
    lufs_from_replaygain, AnalyzerMessage, AnalyzerTap, AnalyzerWaveformTrack, AudioBackendType,
    AudioDiagnostic, AudioSettings, BackendConfig, BackendManager, BitPerfectMode,
    DiagnosticSource, DynamicAmplify, LoudnessAnalyzer, LoudnessCache, LoudnessSource,
    TappedSource, VisualizerTap,
};
use qbz_models::{AssetOrigin, ExternalStreamAsset, Quality, StreamQualityInfo};
use qbz_qobuz::QobuzClient;

/// Commands sent to the audio thread
enum AudioCommand {
    /// Play audio data with track ID, duration, and audio specs
    Play {
        data: Vec<u8>,
        track_id: u64,
        duration_secs: u64,
        sample_rate: u32,
        channels: u16,
        play_gen: u64,
    },
    /// Play from streaming source (BufferedMediaSource)
    /// The download task should already be running and pushing to the source
    PlayStreaming {
        source: Arc<BufferedMediaSource>,
        track_id: u64,
        sample_rate: u32,
        channels: u16,
        duration_secs: u64,
        /// Resume offset in seconds (#315). When > 0, the audio thread
        /// waits for enough buffer to cover the offset and pre-skips
        /// decoder output up to that point before engaging audio.
        start_position_secs: u64,
        /// Total content length in bytes. Combined with `duration_secs`
        /// to estimate bytes-per-second when sizing the resume buffer.
        content_length: u64,
        /// Play generation this command belongs to (PR #583 counter,
        /// snapshotted at send time). The audio thread stops waiting for the
        /// initial buffer as soon as a newer play intent bumps the counter,
        /// instead of blocking up to 60s on a superseded download (#591).
        play_gen: u64,
    },
    /// Pause playback
    Pause,
    /// Resume playback
    Resume,
    /// Stop playback
    Stop { completed: Option<tokio::sync::oneshot::Sender<()>> },
    /// Set volume (0.0 - 1.0)
    SetVolume(f32),
    /// Seek to position in seconds
    Seek(u64),
    /// Reinitialize audio device (releases and re-acquires)
    ReinitDevice { device_name: Option<String> },
    /// Release the output device WITHOUT reopening it: drops the active
    /// stream (freeing an exclusive ALSA `hw:` grab + its D-Bus reservation)
    /// and un-suspends / un-forces anything QBZ parked, so PipeWire can
    /// reclaim a device QBZ was holding. User-triggered from settings.
    ReleaseDevice {
        completed: SyncSender<Result<(), String>>,
    },
    /// Append next track to current engine for gapless playback (Rodio only)
    PlayNext {
        data: Vec<u8>,
        track_id: u64,
        sample_rate: u32,
        channels: u16,
        bit_depth: u32,
    },
    /// Append an incrementally-fed successor to the current engine. Used by
    /// Streaming only: the handoff depends on the initial buffer, not on
    /// downloading a potentially very long track in full inside the 10 s
    /// gapless window.
    PlayNextStreaming {
        source: Arc<BufferedMediaSource>,
        track_id: u64,
        sample_rate: u32,
        channels: u16,
        bit_depth: u32,
        duration_secs: u64,
    },
    /// Play a local DSD file via DoP (DSD over PCM) on ALSA direct (DSD plan
    /// Phase 2). The audio thread opens the demuxer + an S32 stream at the
    /// DoP carrier rate and feeds pre-packed words through the DoP engine.
    PlayDsdDop {
        path: std::path::PathBuf,
        track_id: u64,
    },
    /// Play a local DSD file NATIVELY (ALSA DSD_U32, DSD plan Phase 3) —
    /// requires the kernel to grant the device a DSD format (quirk table).
    PlayDsdNative {
        path: std::path::PathBuf,
        track_id: u64,
    },
    /// Queue the next DSD track on the ACTIVE DoP engine (gapless DSD).
    /// Ignored (with gapless_ready reset) when the engine isn't DoP or the
    /// carrier rate differs — the normal track-end advance then handles it.
    PlayNextDsdDop {
        path: std::path::PathBuf,
        track_id: u64,
    },
}

/// Pending gapless track data (queued for seamless transition)
enum GaplessMedia {
    Buffered(Arc<Vec<u8>>),
    Streaming(Arc<BufferedMediaSource>),
    Direct(DirectDsdMedia),
}

#[derive(Clone)]
struct DirectDsdMedia {
    path: std::path::PathBuf,
    /// 1 = DoP, 2 = native DSD_U32_BE, 3 = native DSD_U32_LE.
    mode: u8,
}

struct GaplessPending {
    track_id: u64,
    play_generation: u64,
    duration_secs: u64,
    media: GaplessMedia,
    sample_rate: u32,
    channels: u16,
    bit_depth: u32,
    normalization_gain: Option<f32>,
    gain_atomic: Option<Arc<AtomicU32>>,
}

struct CursorMediaSource {
    inner: Cursor<Vec<u8>>,
    len: u64,
}

impl CursorMediaSource {
    fn new(data: Vec<u8>) -> Self {
        let len = data.len() as u64;
        Self {
            inner: Cursor::new(data),
            len,
        }
    }
}

impl MediaSource for CursorMediaSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

impl Read for CursorMediaSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for CursorMediaSource {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Audio specifications extracted from decoded audio
#[allow(dead_code)]
struct AudioSpecs {
    samples: SamplesBuffer,
    sample_rate: u32,
    channels: u16,
}

fn cpal_device_name(device: &rodio::cpal::Device) -> Option<String> {
    device
        .description()
        .ok()
        .map(|description| description.name().to_string())
}

fn decode_with_symphonia(data: &[u8]) -> Result<AudioSpecs, String> {
    let source = Box::new(CursorMediaSource::new(data.to_vec())) as Box<dyn MediaSource>;
    let mss = MediaSourceStream::new(source, Default::default());

    let mut hint = Hint::new();
    hint.with_extension("m4a");

    let format_opts = FormatOptions {
        enable_gapless: true,
        ..Default::default()
    };
    let metadata_opts: MetadataOptions = Default::default();
    let mut probed = get_probe()
        .format(&hint, mss, &format_opts, &metadata_opts)
        .map_err(|err| format!("Symphonia probe failed: {}", err))?;

    let track = probed
        .format
        .default_track()
        .ok_or_else(|| "Symphonia: no supported audio tracks".to_string())?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();

    let mut decoder = get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|err| format!("Symphonia decoder init failed: {}", err))?;

    let mut sample_rate = 0;
    let mut channels = 0u16;
    let mut samples: Vec<f32> = Vec::new();

    loop {
        let packet = match probed.format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(_)) => break,
            Err(err) => return Err(format!("Symphonia read error: {}", err)),
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                let spec = *audio_buf.spec();
                if sample_rate == 0 {
                    sample_rate = spec.rate;
                    channels = spec.channels.count() as u16;
                }

                let mut sample_buf = SampleBuffer::<f32>::new(audio_buf.frames() as u64, spec);
                sample_buf.copy_interleaved_ref(audio_buf);
                samples.extend_from_slice(sample_buf.samples());
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(err) => return Err(format!("Symphonia decode error: {}", err)),
        }
    }

    if samples.is_empty() || sample_rate == 0 || channels == 0 {
        return Err("Symphonia decode produced no audio".to_string());
    }

    Ok(AudioSpecs {
        samples: SamplesBuffer::new(
            std::num::NonZero::new(channels).unwrap(),
            std::num::NonZero::new(sample_rate).unwrap(),
            samples,
        ),
        sample_rate,
        channels,
    })
}

fn is_isomp4(data: &[u8]) -> bool {
    if data.len() < 12 {
        return false;
    }

    &data[4..8] == b"ftyp"
}

/// Extract audio metadata (sample rate, channels) without full decode.
/// This is much faster than decode_with_symphonia as it only reads headers.
/// Audio metadata extracted from file headers
#[allow(dead_code)]
pub(crate) struct AudioMetadata {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    pub(crate) bit_depth: Option<u32>,
    pub(crate) codec: CodecType,
}

#[allow(dead_code)]
fn extract_audio_metadata(data: &[u8]) -> Result<(u32, u16), String> {
    let meta = extract_audio_metadata_full(data)?;
    Ok((meta.sample_rate, meta.channels))
}

pub(crate) fn extract_audio_metadata_full(data: &[u8]) -> Result<AudioMetadata, String> {
    // For non-isomp4 files (FLAC, etc.), try symphonia directly to get all metadata
    // Symphonia gives us bits_per_sample which rodio doesn't expose

    // Use symphonia probe for codec params (no decode needed)
    let source = Box::new(CursorMediaSource::new(data.to_vec())) as Box<dyn MediaSource>;
    let mss = MediaSourceStream::new(source, Default::default());

    let mut hint = Hint::new();
    if is_isomp4(data) {
        hint.with_extension("m4a");
    }

    let format_opts = FormatOptions {
        enable_gapless: true,
        ..Default::default()
    };
    let metadata_opts: MetadataOptions = Default::default();
    let probed = get_probe()
        .format(&hint, mss, &format_opts, &metadata_opts)
        .map_err(|err| format!("Symphonia probe failed: {}", err))?;

    let track = probed
        .format
        .default_track()
        .ok_or_else(|| "Symphonia: no supported audio tracks".to_string())?;

    audio_metadata_from_codec_params(&track.codec_params)
}

/// ALAC's codec configuration is authoritative. The MP4 sample entry may
/// contain a placeholder rate (1 Hz in real hi-res files), and Symphonia's
/// demuxer does not propagate ALAC's bit depth into CodecParameters. Its
/// decoder already reads the cookie; every source and device setup must agree.
fn audio_metadata_from_codec_params(
    params: &symphonia::core::codecs::CodecParameters,
) -> Result<AudioMetadata, String> {
    if params.codec == symphonia::core::codecs::CODEC_TYPE_ALAC {
        let cookie = params
            .extra_data
            .as_deref()
            .ok_or("ALAC configuration missing")?;
        // ALACSpecificConfig, optionally followed by a 24-byte channel layout.
        if !matches!(cookie.len(), 24 | 48) {
            return Err("Invalid ALAC configuration length".into());
        }
        let bit_depth = u32::from(cookie[5]);
        let channels = u16::from(cookie[9]);
        let sample_rate = u32::from_be_bytes(cookie[20..24].try_into().unwrap());
        if cookie[4] != 0
            || !(1..=32).contains(&bit_depth)
            || !(1..=8).contains(&channels)
            || sample_rate == 0
        {
            return Err("Invalid ALAC signal format".into());
        }
        return Ok(AudioMetadata {
            sample_rate,
            channels,
            bit_depth: Some(bit_depth),
            codec: params.codec,
        });
    }
    let sample_rate = params.sample_rate.ok_or("No sample rate in codec params")?;
    let channels = params.channels.map(|c| c.count() as u16).unwrap_or(2);
    // Same guard the fully-decoded path applies (`samples.is_empty() ||
    // sample_rate == 0 || channels == 0`): the streaming sources build
    // `NonZero` from these with an `unwrap`.
    if sample_rate == 0 || channels == 0 {
        return Err("Invalid signal format in codec params (zero sample rate or channel count)".into());
    }
    Ok(AudioMetadata {
        sample_rate,
        channels,
        bit_depth: params.bits_per_sample,
        codec: params.codec,
    })
}

/// True when a cached FLAC is a lower quality than `requested`, so the
/// cache entry should be bypassed and the track re-fetched. Ported from
/// the Tauri `cached_quality_below_requested` helper: an unparseable
/// buffer is assumed compatible.
fn cached_quality_below_requested(data: &[u8], requested: Quality) -> bool {
    let meta = match extract_audio_metadata_full(data) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let sample_rate = meta.sample_rate;
    let bit_depth = meta.bit_depth.unwrap_or(16);
    match requested {
        // Hi-Res+: expect 24-bit AND > 96 kHz.
        Quality::UltraHiRes => bit_depth < 24 || sample_rate <= 96000,
        // Hi-Res: expect 24-bit.
        Quality::HiRes => bit_depth < 24,
        // Lossless / Mp3: any FLAC satisfies the request.
        _ => false,
    }
}

fn decode_with_fallback(data: &[u8]) -> Result<Box<dyn Source<Item = f32> + Send>, String> {
    if is_isomp4(data) {
        return decode_with_symphonia(data).map(|specs| {
            log::info!("Decoded audio using symphonia fallback (isomp4)");
            Box::new(specs.samples) as Box<dyn Source<Item = f32> + Send>
        });
    }

    // Hand rodio the buffer length. `Decoder::new` leaves `byte_len` unset
    // and the source non-seekable, and symphonia's MP3 demuxer only estimates
    // the duration of a file without a Xing/Info/VBRI header when both are
    // known — otherwise `total_duration()` is None and the local play path
    // ends up with duration 0 (position clamped to 0 forever, #734).
    let byte_len = data.len() as u64;
    let primary = panic::catch_unwind(AssertUnwindSafe(|| {
        Decoder::builder()
            .with_data(BufReader::new(Cursor::new(data.to_vec())))
            .with_byte_len(byte_len)
            .build()
    }));

    match primary {
        Ok(Ok(decoder)) => return Ok(Box::new(decoder)),
        Ok(Err(err)) => {
            log::warn!("Primary decode failed, attempting mp4 fallback: {}", err);
        }
        Err(_) => {
            log::warn!("Primary decode panicked, attempting mp4 fallback");
        }
    }

    // Try mp4 fallback (rodio 0.22 removed Mp4Type hint)
    {
        let attempt = panic::catch_unwind(AssertUnwindSafe(|| {
            Decoder::new_mp4(BufReader::new(Cursor::new(data.to_vec())))
        }));

        match attempt {
            Ok(Ok(decoder)) => {
                log::info!("Decoded audio using mp4 fallback");
                return Ok(Box::new(decoder));
            }
            Ok(Err(err)) => {
                log::warn!("mp4 fallback failed: {}", err);
            }
            Err(_) => {
                log::warn!("mp4 fallback panicked");
            }
        }
    }

    match decode_with_symphonia(data) {
        Ok(specs) => {
            log::info!("Decoded audio using symphonia fallback");
            Ok(Box::new(specs.samples))
        }
        Err(err) => Err(err),
    }
}

/// Create MixerDeviceSink with custom sample rate configuration
fn create_output_stream_with_config(
    device: rodio::cpal::Device,
    sample_rate: u32,
    channels: u16,
    exclusive_mode: bool,
    state: SharedState,
) -> Result<MixerDeviceSink, String> {
    log::info!(
        "Creating MixerDeviceSink: {}Hz, {} channels, exclusive: {}",
        sample_rate,
        channels,
        exclusive_mode
    );

    // Create StreamConfig with desired sample rate
    // Note: buffer_size here is unused — with_supported_config() resets it.
    // The actual buffer size is set via with_buffer_size() below.
    let config = StreamConfig {
        channels,
        sample_rate,
        buffer_size: BufferSize::Default,
    };

    // Check if device supports this configuration
    let supported_configs = device
        .supported_output_configs()
        .map_err(|e| format!("Failed to get supported configs: {}", e))?;

    let mut found_matching = false;
    for range in supported_configs {
        if range.channels() == channels
            && sample_rate >= range.min_sample_rate()
            && sample_rate <= range.max_sample_rate()
        {
            found_matching = true;
            log::info!(
                "Device supports {}Hz (range: {}-{}Hz)",
                sample_rate,
                range.min_sample_rate(),
                range.max_sample_rate()
            );
            break;
        }
    }

    if !found_matching {
        log::warn!(
            "Device may not support {}Hz, attempting anyway",
            sample_rate
        );
    }

    // Create SupportedStreamConfig
    let supported_config = SupportedStreamConfig::new(
        config.channels,
        config.sample_rate,
        SupportedBufferSize::Range { min: 64, max: 8192 },
        SampleFormat::F32,
    );

    // Compute buffer size — must be applied AFTER with_supported_config()
    // because that method resets buffer_size to Default via ..Default::default().
    // MixerDeviceSink has zero internal buffering, so CPAL's buffer is the
    // ONLY buffer between the mixer and audio hardware.
    let cpal_buffer_size = if exclusive_mode {
        BufferSize::Fixed(512) // Low latency for exclusive mode
    } else {
        // ~100ms buffer, matching old vendored cpal period size.
        // Prevents underruns at high sample rates (192kHz = 19200 frames).
        BufferSize::Fixed(sample_rate / 10)
    };
    log::info!("Buffer size: {:?}", cpal_buffer_size);

    // Create MixerDeviceSink with custom config
    match DeviceSinkBuilder::from_device(device) {
        Ok(builder) => {
            // rodio's default error callback only eprintln!s a live stream
            // error (e.g. an ALSA buffer underrun mid-playback), so playback
            // dies without the driver ever seeing a message to latch. Record
            // it on the shared state instead so it drains to the event bus as
            // a PlaybackError. Rate-limited: ALSA can repeat EPIPE every
            // period on a wedged device, and each recorded message is one
            // bus event (and one forked hook script) after the drain.
            //
            // #660: the same limiter also caps the LOG line (cpal fires the
            // POLLERR error ~1.7 M times per second on a wedged PCM), and a
            // wedge is latched once for the audio thread to rebuild the
            // stream — see qbz_audio::stream_health.
            let mut limiter = qbz_audio::stream_health::FloodLimiter::new(
                qbz_audio::stream_health::LOG_WINDOW,
            );
            let mut wedge_latched = false;
            match builder
                .with_supported_config(&supported_config)
                .with_buffer_size(cpal_buffer_size)
                .with_error_callback(move |err| {
                    let fault = qbz_audio::stream_health::classify(&err);
                    if let Some(suppressed) = limiter.admit(std::time::Instant::now()) {
                        if suppressed == 0 {
                            log::error!("Audio stream error: {err}");
                        } else {
                            log::error!(
                                "Audio stream error: {err} ({suppressed} more suppressed)"
                            );
                        }
                        state.record_stream_error(format!("Audio stream error: {err}"));
                    }
                    if let qbz_audio::stream_health::StreamFault::Wedged(reason) = fault {
                        if !wedge_latched {
                            wedge_latched = true;
                            qbz_audio::stream_health::raise_wedged(format!("CPAL: {reason}"));
                        }
                    }
                })
                .open_stream()
            {
                Ok(mixer_sink) => {
                    log::info!("MixerDeviceSink created successfully at {}Hz", sample_rate);
                    Ok(mixer_sink)
                }
                Err(e) => {
                    log::error!("Failed to open stream at {}Hz: {}", sample_rate, e);
                    Err(format!("Failed to create output stream: {}", e))
                }
            }
        }
        Err(e) => {
            log::error!("Failed to create device sink builder: {}", e);
            Err(format!("Failed to create output stream: {}", e))
        }
    }
}

/// Output stream type - either rodio or ALSA Direct.
///
/// On macOS, the Rodio variant carries an optional `exclusive_guard`
/// whose `Drop` releases CoreAudio Hog Mode and is otherwise inert —
/// so exclusive and shared modes share a single code path.
enum StreamType {
    Rodio {
        sink: MixerDeviceSink,
        /// Holds CoreAudio Hog Mode for the lifetime of the stream.
        /// Load-bearing via `Drop`; reads happen through pattern matches
        /// (e.g., `set_coreaudio_hardware_volume`).
        #[cfg(target_os = "macos")]
        exclusive_guard: Option<qbz_audio::CoreAudioExclusiveGuard>,
    },
    /// Any bit-perfect DIRECT sink: ALSA hw: on Linux, WASAPI exclusive on
    /// Windows. One variant rather than one per platform, because everything
    /// downstream treats them identically - see `qbz_audio::backend::DirectSink`.
    ///
    /// NOT cfg-gated: gating it to Linux is what made the Windows compiler
    /// unable to check any of the arms that handle it, which is the whole
    /// blind spot this port has to work around.
    Direct(Arc<dyn qbz_audio::backend::DirectSink>),
    /// Native JACK output (#263 Tier 3). QBZ as a JACK client with stable ports;
    /// NOT bit-perfect (resampled to the graph rate).
    #[cfg(target_os = "linux")]
    Jack(Arc<qbz_audio::JackStream>),
}

impl StreamType {
    fn output_format(&self) -> (u32, u16) {
        match self {
            Self::Rodio { sink, .. } => (
                sink.config().sample_rate().get(),
                sink.config().channel_count().get(),
            ),
            Self::Direct(sink) => (sink.sample_rate(), sink.channels()),
            #[cfg(target_os = "linux")]
            Self::Jack(sink) => (sink.sample_rate(), sink.channels()),
        }
    }

    /// Construct a shared-mode Rodio stream (no exclusive guard).
    fn rodio(sink: MixerDeviceSink) -> Self {
        StreamType::Rodio {
            sink,
            #[cfg(target_os = "macos")]
            exclusive_guard: None,
        }
    }

    /// Apply the volume to CoreAudio hardware if the device supports it.
    ///
    /// Returns `true` only when the hardware accepted the change so the
    /// caller can pin the software stream to unity gain. Returns `false`
    /// for shared mode and for knob-only DACs (no settable volume
    /// property), letting the caller fall back to software volume.
    #[cfg(target_os = "macos")]
    fn set_coreaudio_hardware_volume(&self, volume: f32) -> bool {
        match self {
            StreamType::Rodio {
                exclusive_guard: Some(guard),
                ..
            } => match guard.set_hardware_volume(volume) {
                Ok(()) => true,
                Err(e) => {
                    log::warn!(
                        "[CoreAudio] Hardware volume failed; falling back to software: {}",
                        e
                    );
                    false
                }
            },
            _ => false,
        }
    }

    /// Actual output stream rate. On macOS shared mode this is the rate that
    /// must match CoreAudio's current nominal device rate; decoded track rates
    /// may differ and are resampled by Rodio.
    #[cfg(target_os = "macos")]
    fn output_sample_rate(&self) -> u32 {
        match self {
            StreamType::Rodio { sink, .. } => sink.config().sample_rate().get(),
            // `Direct` is deliberately not cfg-gated (see the enum), so this
            // macOS-only match must name it: a direct sink reports the rate it
            // was opened at. Without this arm the macOS build does not compile.
            StreamType::Direct(sink) => sink.sample_rate(),
        }
    }
}

fn apply_engine_volume(
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] stream_opt: &Option<StreamType>,
    engine: &PlaybackEngine,
    volume: f32,
) {
    #[cfg(target_os = "macos")]
    if stream_opt
        .as_ref()
        .map(|stream| stream.set_coreaudio_hardware_volume(volume))
        .unwrap_or(false)
    {
        engine.set_volume(1.0);
        return;
    }

    engine.set_volume(volume);
}

fn reported_volume_after_command(
    requested: f32,
    direct_output: bool,
    hardware_volume_active: bool,
) -> f32 {
    #[cfg(target_os = "linux")]
    if direct_output && !hardware_volume_active {
        return 1.0;
    }

    #[cfg(not(target_os = "linux"))]
    let _ = (direct_output, hardware_volume_active);

    requested
}

fn selected_alsa_hardware_volume_control(
    settings: &Arc<Mutex<AudioSettings>>,
) -> Option<qbz_audio::alsa_hardware_volume::AlsaMixerControlId> {
    settings
        .lock()
        .ok()
        .and_then(|settings| settings.selected_alsa_hardware_volume_control())
}

fn hardware_volume_event_callback(
    state: &SharedState,
    initially_active: bool,
) -> qbz_audio::alsa_hardware_volume::HardwareVolumeEventCallback {
    use qbz_audio::alsa_hardware_volume::{HardwareVolumeEvent, HardwareVolumeEventCallback};

    state.set_hardware_volume_active(initially_active);
    let state = state.clone();
    // A session that has reported Unavailable must never be revived by an
    // already-queued ctl Changed event. Serializing callbacks also guarantees
    // that whichever event obtains this latch first completes before the next
    // one updates SharedState.
    let session_active = Mutex::new(initially_active);
    let callback: HardwareVolumeEventCallback = Arc::new(move |event| {
        let Ok(mut active) = session_active.lock() else {
            state.set_hardware_volume_active(false);
            state.volume.store(1.0_f32.to_bits(), Ordering::SeqCst);
            state.record_stream_error(
                "ALSA hardware volume event state became unavailable. Direct playback remains fixed at 100%."
                    .to_string(),
            );
            return;
        };
        match event {
            HardwareVolumeEvent::Changed(snapshot) if *active => {
                state
                    .volume
                    .store(snapshot.volume.to_bits(), Ordering::SeqCst);
                state.set_hardware_volume_active(true);
            }
            HardwareVolumeEvent::Changed(_) => {}
            HardwareVolumeEvent::Unavailable(error) => {
                *active = false;
                state.set_hardware_volume_active(false);
                state.volume.store(1.0_f32.to_bits(), Ordering::SeqCst);
                state.record_stream_error(format!(
                    "ALSA hardware volume became unavailable: {error}. Direct playback remains fixed at 100%."
                ));
            }
        }
    });
    callback
}

fn direct_hardware_volume_binding(
    settings: &Arc<Mutex<AudioSettings>>,
    state: &SharedState,
) -> (
    Option<qbz_audio::alsa_hardware_volume::AlsaMixerControlId>,
    qbz_audio::alsa_hardware_volume::HardwareVolumeEventCallback,
) {
    let control = selected_alsa_hardware_volume_control(settings);
    let callback = hardware_volume_event_callback(state, control.is_some());
    (control, callback)
}

#[cfg(target_os = "macos")]
fn uses_coreaudio_system_default(settings: &AudioSettings) -> bool {
    settings
        .backend_type
        .unwrap_or(AudioBackendType::SystemDefault)
        == AudioBackendType::SystemDefault
}

/// `evaluate_stream_recreate` runs every track change on the audio thread,
/// and `coreaudio_nominal_rate` issues two CoreAudio HAL queries per call
/// (`resolve_output_device_id` + `get_nominal_sample_rate`). A short-lived
/// cache absorbs the bulk of those repeats — long enough to skip duplicate
/// queries within an album, short enough to notice when the user changes the
/// system audio rate.
#[cfg(target_os = "macos")]
const COREAUDIO_RATE_CACHE_TTL: std::time::Duration = std::time::Duration::from_millis(750);

#[cfg(target_os = "macos")]
struct CachedNominalRate {
    cached_at: std::time::Instant,
    device_name: Option<String>,
    rate: Option<u32>,
}

#[cfg(target_os = "macos")]
std::thread_local! {
    static COREAUDIO_NOMINAL_RATE_CACHE: std::cell::RefCell<Option<CachedNominalRate>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(target_os = "macos")]
fn query_macos_nominal_rate(device_name: Option<&str>) -> Option<u32> {
    let device_id = qbz_audio::coreaudio_direct::resolve_output_device_id(device_name)
        .inspect_err(|e| {
            log::warn!(
                "[CoreAudio] Failed to resolve output device for rate check: {}",
                e
            )
        })
        .ok()?;

    qbz_audio::coreaudio_direct::get_nominal_sample_rate(device_id)
        .inspect_err(|e| log::warn!("[CoreAudio] Failed to query nominal rate: {}", e))
        .ok()
}

#[cfg(target_os = "macos")]
fn coreaudio_nominal_rate(settings: &AudioSettings) -> Option<u32> {
    if !uses_coreaudio_system_default(settings) {
        return None;
    }

    let device_name = settings.output_device.clone();
    let now = std::time::Instant::now();

    COREAUDIO_NOMINAL_RATE_CACHE.with(|cell| {
        let mut cell = cell.borrow_mut();
        if let Some(cached) = cell.as_ref() {
            if cached.device_name == device_name
                && now.duration_since(cached.cached_at) < COREAUDIO_RATE_CACHE_TTL
            {
                return cached.rate;
            }
        }
        let rate = query_macos_nominal_rate(device_name.as_deref());
        *cell = Some(CachedNominalRate {
            cached_at: now,
            device_name,
            rate,
        });
        rate
    })
}

#[cfg(target_os = "macos")]
fn coreaudio_shared_rate_mismatch(
    settings: &AudioSettings,
    stream_opt: &Option<StreamType>,
) -> Option<(u32, u32)> {
    if settings.exclusive_mode || !uses_coreaudio_system_default(settings) {
        return None;
    }

    let stream_rate = stream_opt.as_ref().map(StreamType::output_sample_rate)?;
    let nominal_rate = coreaudio_nominal_rate(settings)?;

    (stream_rate != nominal_rate).then_some((stream_rate, nominal_rate))
}

/// How the audio thread answers a wedged output stream (#660): cpal's ALSA
/// worker spins on `alsa::poll() returned POLLERR` (a PCM left in XRUN /
/// SUSPENDED / DISCONNECTED state after CPU starvation, a suspend, or a
/// device that went away) and never recovers; dropping the stream is the only
/// exit. The first wedge is rebuilt at once; a second one inside the backoff
/// window means the device itself is broken, so the thread releases it and
/// stops instead of rebuilding in a loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WedgeRecovery {
    Rebuild,
    Stop,
}

const WEDGE_REBUILD_BACKOFF: Duration = Duration::from_secs(10);

fn wedge_recovery(last_rebuild: Option<Instant>, now: Instant) -> WedgeRecovery {
    match last_rebuild {
        Some(last) if now.duration_since(last) < WEDGE_REBUILD_BACKOFF => WedgeRecovery::Stop,
        _ => WedgeRecovery::Rebuild,
    }
}

/// Inputs needed by both `Play` and `Stream` handlers to decide whether the
/// current output stream must be torn down and recreated.
struct StreamRecreateDecision {
    needs_new_stream: bool,
    format_changed: bool,
    dac_passthrough: bool,
    using_alsa_direct: bool,
    using_coreaudio_exclusive: bool,
    coreaudio_shared_rate_mismatch: Option<(u32, u32)>,
}

/// Read settings once and evaluate every condition that forces a stream
/// rebuild: any decoded-format change (sample rate / channels — the output
/// stream must follow the track's native rate on every backend, #449) and
/// CoreAudio shared-mode rate drift (CPAL caches the device rate at open
/// time, so when the OS nominal rate moves we must rebuild or playback runs
/// at the wrong speed).
///
/// `current_track_sample_rate` / `current_track_channels` describe the last
/// *decoded source* format and are compared to the incoming `sample_rate` /
/// `channels` — never to the output stream's hardware rate, which on macOS
/// shared mode may differ and is tracked separately via Rodio's sink config.
fn evaluate_stream_recreate(
    thread_settings: &Arc<Mutex<AudioSettings>>,
    stream_opt: &Option<StreamType>,
    current_track_sample_rate: Option<u32>,
    current_track_channels: Option<u16>,
    sample_rate: u32,
    channels: u16,
    context: &str,
) -> StreamRecreateDecision {
    let format_changed =
        current_track_sample_rate != Some(sample_rate) || current_track_channels != Some(channels);

    let settings_guard = thread_settings.lock().ok();

    let dac_passthrough = settings_guard
        .as_ref()
        .map(|s| cfg!(target_os = "linux") && s.dac_passthrough)
        .unwrap_or(false);

    let using_alsa_direct = settings_guard
        .as_ref()
        .and_then(|s| s.backend_type)
        .map(|b| b == AudioBackendType::Alsa)
        .unwrap_or(false);

    let using_coreaudio_exclusive = settings_guard
        .as_ref()
        .map(|s| {
            cfg!(target_os = "macos")
                && s.backend_type.unwrap_or(AudioBackendType::SystemDefault)
                    == AudioBackendType::SystemDefault
                && s.exclusive_mode
        })
        .unwrap_or(false);

    #[cfg(target_os = "macos")]
    let coreaudio_shared_rate_mismatch = settings_guard
        .as_ref()
        .and_then(|s| coreaudio_shared_rate_mismatch(s, stream_opt))
        .inspect(|(stream_rate, nominal_rate)| {
            log::warn!(
                "[CoreAudio] {} shared-mode output rate changed: stream {}Hz, device nominal {}Hz. Recreating stream to avoid wrong-speed playback.",
                context,
                stream_rate,
                nominal_rate
            );
        });
    #[cfg(not(target_os = "macos"))]
    let coreaudio_shared_rate_mismatch: Option<(u32, u32)> = {
        let _ = (stream_opt, context);
        None
    };

    drop(settings_guard);

    let needs_new_stream = compute_needs_new_stream(
        stream_opt.is_some(),
        format_changed,
        dac_passthrough,
        using_alsa_direct,
        using_coreaudio_exclusive,
        coreaudio_shared_rate_mismatch.is_some(),
    );

    StreamRecreateDecision {
        needs_new_stream,
        format_changed,
        dac_passthrough,
        using_alsa_direct,
        using_coreaudio_exclusive,
        coreaudio_shared_rate_mismatch,
    }
}

/// Pure decision rule for whether the output stream must be rebuilt.
///
/// Split out so the truth table can be unit-tested without faking a real
/// `MixerDeviceSink` or `AudioSettings` mutex.
fn compute_needs_new_stream(
    has_stream: bool,
    format_changed: bool,
    _dac_passthrough: bool,
    _using_alsa_direct: bool,
    _using_coreaudio_exclusive: bool,
    coreaudio_shared_rate_mismatch: bool,
) -> bool {
    // A decoded-format change (sample rate or channel count) requires a fresh
    // output stream on EVERY backend so the device follows the track's native
    // rate (#449). The bit-perfect flags used to gate this, which was correct
    // only while Stop dropped the stream; once that drop was deferred to avoid
    // a track-change click (e93fcaec), the default/PipeWire path stopped
    // switching rates and stayed locked to the first track. Same-rate tracks
    // keep reusing the stream (format_changed == false), preserving the click
    // fix.
    !has_stream || format_changed || coreaudio_shared_rate_mismatch
}

/// Try to create output stream using the backend system (if configured)
/// Returns None if backend system is not configured (backend_type = None)
///
/// For ALSA backend with hw: devices, may return AlsaDirect instead of Rodio stream.
fn try_init_stream_with_backend(
    audio_settings: &AudioSettings,
    sample_rate: u32,
    channels: u16,
    state: &SharedState,
) -> Option<Result<StreamType, String>> {
    // A None backend_type means "Auto" / unset. Resolve it to SystemDefault on
    // every platform instead of returning None — returning None made the caller
    // fall through to the legacy CPAL path, which forced the track rate onto the
    // shared default device: that froze the seekbar with no audio AND left a
    // process-wide stuck audio handle that survived Reset (#470). "Auto" is
    // resolved to a concrete backend in the UI; this is the backend-side safety
    // net for any remaining None (legacy installs, headless callers).
    let backend_type = audio_settings
        .backend_type
        .unwrap_or(qbz_audio::AudioBackendType::SystemDefault);

    log::info!(
        "Using backend system: {:?} (device: {:?}, plugin: {:?})",
        backend_type,
        audio_settings.output_device,
        audio_settings.alsa_plugin
    );

    // Create backend
    let backend = match BackendManager::create_backend(backend_type) {
        Ok(b) => b,
        Err(e) => {
            log::error!("Failed to create backend {:?}: {}", backend_type, e);
            return Some(Err(e));
        }
    };

    // Check availability
    if !backend.is_available() {
        let msg = format!("Backend {:?} is not available on this system", backend_type);
        log::error!("{}", msg);
        return Some(Err(msg));
    }

    // Build backend config
    let config = BackendConfig {
        backend_type,
        device_id: audio_settings.output_device.clone(),
        sample_rate,
        channels,
        exclusive_mode: audio_settings.exclusive_mode,
        alsa_plugin: audio_settings.alsa_plugin,
        pw_force_bitperfect: audio_settings.pw_force_bitperfect,
        skip_sink_switch: audio_settings.skip_sink_switch,
    };

    // WASAPI EXCLUSIVE (Windows) - the bit-perfect path, tried before the
    // shared CPAL stream exactly as ALSA Direct is on Linux.
    //
    // The device id is the WASAPI ENDPOINT ID, which is what the enumeration
    // already publishes: Windows hands several endpoints the same friendly
    // name, so the name cannot identify one. Without a chosen device there is
    // nothing to open exclusively - falling through to shared is the honest
    // answer, not picking the default DAC behind the user's back.
    #[cfg(windows)]
    if backend_type == AudioBackendType::WasapiExclusive {
        match config.device_id.as_ref() {
            None => {
                log::info!(
                    "[WASAPI] Exclusive mode selected with no device chosen; using the shared stream. Pick an output device to get bit-perfect playback."
                );
            }
            Some(endpoint_id) => {
                use qbz_audio::wasapi_direct::{WasapiDirectStream, WasapiTiming};
                match WasapiDirectStream::new(
                    endpoint_id,
                    sample_rate,
                    config.channels,
                    WasapiTiming::Events,
                ) {
                    Ok(stream) => {
                        let mode = stream.bit_perfect_mode();
                        let info = stream.open_info();
                        log::info!(
                            "[WASAPI] Exclusive stream open: {} @ {} Hz, {:?}, mode {:?}",
                            info.endpoint_name,
                            info.rate,
                            info.rung,
                            mode
                        );
                        state.set_bit_perfect_mode(Some(mode));
                        return Some(Ok(StreamType::Direct(Arc::new(stream))));
                    }
                    Err(e) => {
                        // NOT fatal: a device that refuses this rate in
                        // exclusive mode still plays through the shared path,
                        // and silence would be a worse answer than a
                        // resampled track.
                        log::warn!(
                            "[WASAPI] Exclusive open failed ({e}); falling back to the shared stream"
                        );
                    }
                }
            }
        }
    }

    // For ALSA backend with hw: devices, try direct ALSA first (Linux only)
    #[cfg(target_os = "linux")]
    if backend_type == AudioBackendType::Alsa {
        // Check if device is hw: or plughw:
        if let Some(ref device_id) = config.device_id {
            if qbz_audio::AlsaDirectStream::is_hw_device(device_id) {
                log::info!("Detected hw: device, using ALSA Direct for bit-perfect playback");

                // Downcast backend to AlsaBackend to access try_create_direct_stream
                if let Some(alsa_backend) = backend
                    .as_any()
                    .downcast_ref::<qbz_audio::alsa_backend::AlsaBackend>()
                {
                    if let Some(result) = alsa_backend.try_create_direct_stream(&config) {
                        return Some(result.map(|(stream, mode)| {
                            log::info!("ALSA Direct stream created with mode: {:?}", mode);
                            state.set_bit_perfect_mode(Some(mode));
                            StreamType::Direct(Arc::new(stream))
                        }));
                    }
                }
            }
        }
    }

    // JACK (#263 Tier 3): create the JACK client/stream directly (not via the
    // MixerDeviceSink trait). Opt-in routing-freedom mode, NOT bit-perfect.
    #[cfg(target_os = "linux")]
    if backend_type == AudioBackendType::Jack {
        match qbz_audio::JackStream::new(config.channels) {
            Ok(stream) => {
                state.set_bit_perfect_mode(Some(qbz_audio::BitPerfectMode::Disabled));
                return Some(Ok(StreamType::Jack(Arc::new(stream))));
            }
            Err(e) => return Some(Err(format!("JACK backend unavailable: {e}"))),
        }
    }

    // Fallback to regular rodio stream (PipeWire, Pulse, ALSA via CPAL)
    match backend.create_output_stream_with_exclusive_guard(&config) {
        Ok((mixer_sink, _exclusive_guard)) => {
            let output_sample_rate = mixer_sink.config().sample_rate().get();
            log::info!(
                "Stream created via {:?} backend (requested {}Hz, output {}Hz)",
                backend_type,
                sample_rate,
                output_sample_rate
            );
            state.set_bit_perfect_mode(Some(BitPerfectMode::Disabled));
            #[cfg(target_os = "macos")]
            let stream = if backend_type == AudioBackendType::SystemDefault {
                StreamType::Rodio {
                    sink: mixer_sink,
                    exclusive_guard: _exclusive_guard,
                }
            } else {
                StreamType::rodio(mixer_sink)
            };
            #[cfg(not(target_os = "macos"))]
            let stream = StreamType::rodio(mixer_sink);
            Some(Ok(stream))
        }
        Err(e) => {
            log::error!("Backend stream creation failed: {}", e);
            Some(Err(e))
        }
    }
}

/// A Qobuz track being downloaded WHILE an external renderer reads it
/// (`Player::open_external_stream`). Sending `cancel` aborts the download;
/// the buffer is shared with the media server's range readers.
pub struct ExternalStreamHandle {
    pub source: Arc<BufferedMediaSource>,
    /// Exact final FLAC byte length (Content-Length for the renderer).
    pub total_bytes: u64,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub cancel: tokio::sync::watch::Sender<bool>,
}

/// Player-owned buffer lifecycle. This is intentionally independent from
/// QConnect: adapters map it to their wire protocol instead of teaching the
/// audio engine about renderer constants.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub enum PlaybackBufferState {
    #[default]
    Idle,
    InitialBuffering,
    Ready,
    Underrun,
    Error,
}

#[derive(Debug, Clone, Copy)]
struct PlaybackBufferSlot {
    state: PlaybackBufferState,
    track_id: u64,
    play_generation: u64,
}

impl Default for PlaybackBufferSlot {
    fn default() -> Self {
        Self {
            state: PlaybackBufferState::Idle,
            track_id: 0,
            play_generation: 0,
        }
    }
}

/// Generation-scoped reporter carried by a live incremental decoder.
/// Reporting is a side channel only; it never changes PCM flow or scheduling.
#[derive(Clone)]
pub(super) struct PlaybackBufferReporter {
    state: SharedState,
    track_id: u64,
    play_generation: u64,
}

impl PlaybackBufferReporter {
    fn new(state: SharedState, track_id: u64, play_generation: u64) -> Self {
        Self {
            state,
            track_id,
            play_generation,
        }
    }

    pub(super) fn report(&self, buffer_state: PlaybackBufferState) {
        self.state.set_buffer_state_for_play(
            buffer_state,
            self.track_id,
            self.play_generation,
        );
    }
}

/// Event payload for playback state updates
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PlaybackEvent {
    pub is_playing: bool,
    pub position: u64,
    pub duration: u64,
    pub track_id: u64,
    pub volume: f32,
    /// Whether the exact persisted ALSA mixer control is active for this
    /// session. False after a ctl/write/disconnect failure (fail closed).
    #[serde(default)]
    pub hardware_volume_active: bool,
    /// Actual sample rate of the current stream (Hz)
    pub sample_rate: Option<u32>,
    /// Actual bit depth of the current stream
    pub bit_depth: Option<u32>,
    /// Queue shuffle state
    pub shuffle: Option<bool>,
    /// Queue repeat mode ("off", "all", "one")
    pub repeat: Option<String>,
    /// Normalization gain factor being applied (None = normalization not active)
    pub normalization_gain: Option<f32>,
    /// True when backend wants the next track pre-queued for gapless playback
    #[serde(default)]
    pub gapless_ready: bool,
    /// Track ID of the gapless-queued next track (0 = none queued)
    #[serde(default)]
    pub gapless_next_track_id: u64,
    /// Bit-perfect mode of the current stream. None when no stream is active.
    /// Lets the UI show whether playback is direct-hardware bit-perfect, going
    /// through plughw software resample, or running on a shared system path
    /// (pipewire/pulse/cpal) where bit-perfect is not guaranteed.
    #[serde(default)]
    pub bit_perfect_mode: Option<BitPerfectMode>,
    /// Streaming buffer progress (0.0..1.0). `None` when not streaming or
    /// the track is fully buffered — drives the seek-bar cache overlay.
    #[serde(default)]
    pub buffer_progress: Option<f32>,
    /// Instantaneous player-owned buffer lifecycle. Unlike `buffer_progress`,
    /// this remains meaningful when content length is unknown or fully cached.
    #[serde(default)]
    pub buffer_state: PlaybackBufferState,
    /// Track identity paired atomically with `buffer_state`. During initial
    /// buffering it can lead `track_id`, which changes only after audio starts.
    #[serde(default)]
    pub buffer_track_id: u64,
    /// Monotonic edge raised when the audio engine consumes every queued
    /// source. Pollers compare generations instead of trying to sample a
    /// potentially sub-tick `playing -> stopped` transition.
    #[serde(default)]
    pub engine_empty_generation: u64,
    /// Track whose engine-empty edge raised `engine_empty_generation`.
    #[serde(default)]
    pub engine_empty_track_id: u64,
    /// Monotonic edge raised after the live CMAF feeder exhausts its bounded
    /// fetch retries. This also covers failure before playback ever starts.
    #[serde(default)]
    pub source_failure_generation: u64,
    /// Track whose feeder raised `source_failure_generation`.
    #[serde(default)]
    pub source_failure_track_id: u64,
}

/// Shared state between main thread and audio thread
#[derive(Clone)]
pub struct SharedState {
    /// Is currently playing
    is_playing: Arc<AtomicBool>,
    /// Current position in seconds
    position: Arc<AtomicU64>,
    /// Total duration in seconds
    duration: Arc<AtomicU64>,
    /// Current track ID
    current_track_id: Arc<AtomicU64>,
    /// DSD-direct mode: 0 = none, 1 = DoP, 2 = native DSD_U32_BE,
    /// 3 = native DSD_U32_LE. Non-zero means volume is fixed and seek is
    /// unsupported; the gapless arm uses it to build the matching packing.
    dsd_direct: Arc<std::sync::atomic::AtomicU8>,
    /// True when audio data/source is available for playback or resume
    has_loaded_audio: Arc<AtomicBool>,
    /// Volume (0.0 - 1.0, f32 stored as u32 bits — same idiom as
    /// `normalization_gain`; integer-percent storage quantized the volume
    /// to 1% on every re-apply)
    volume: Arc<AtomicU32>,
    /// Session state, separate from the persisted preference. Any control
    /// failure flips this off while direct samples remain at unity.
    hardware_volume_active: Arc<AtomicBool>,
    /// Playback start time (Unix timestamp millis when started/resumed)
    playback_start_millis: Arc<AtomicU64>,
    /// Position when playback was started/resumed (in seconds)
    position_at_start: Arc<AtomicU64>,
    /// Current output device name
    current_device: Arc<std::sync::RwLock<Option<String>>>,
    /// Stream error flag (set when ALSA/audio errors are detected)
    stream_error: Arc<AtomicBool>,
    /// Optional user-readable explanation paired with `stream_error`.
    /// Drained by the Tauri polling loop to emit a frontend toast and then
    /// cleared, so the UI fires the notification exactly once per error.
    stream_error_message: Arc<std::sync::RwLock<Option<String>>>,
    /// Actual sample rate of the current stream (Hz)
    sample_rate: Arc<AtomicU32>,
    /// Actual bit depth of the current stream
    bit_depth: Arc<AtomicU32>,
    /// Current normalization gain factor (f32 stored as u32 bits, 0 = not applied)
    normalization_gain: Arc<AtomicU32>,
    /// True when the audio thread wants the next track pre-queued for gapless
    gapless_ready: Arc<AtomicBool>,
    /// Track ID of the gapless-queued next track (0 = none)
    gapless_next_track_id: Arc<AtomicU64>,
    /// Streaming buffer progress (0.0-1.0 stored as f32 bits, 0 = not streaming)
    buffer_progress: Arc<AtomicU32>,
    /// Buffer state plus the play identity allowed to mutate it. A mutex keeps
    /// the generation check and transition atomic with respect to a new play.
    buffer_state: Arc<std::sync::Mutex<PlaybackBufferSlot>>,
    /// Current bit-perfect mode encoded as u8 (see `bit_perfect_mode_from_u8`).
    /// 0 = Unknown (no stream active yet), 1 = Disabled (CPAL/Rodio / shared
    /// system path), 2 = DirectHardware (ALSA hw:), 3 = PluginFallback (plughw:).
    bit_perfect_mode: Arc<AtomicU8>,
    // Actual QBZ-owned stream configuration, packed atomically: rate + channels.
    // Does not claim the physical DAC format downstream of a system mixer.
    output_stream_format: Arc<AtomicU64>,
    /// Monotonic play generation (PR #583). Bumped by `Player::begin_play` on
    /// every new play intent. Lives in the shared state so the audio thread
    /// can detect that a queued `PlayStreaming` was superseded by a newer play
    /// and stop waiting on its initial buffer instead of blocking ~60s (#591).
    play_generation: Arc<AtomicU64>,
    /// Monotonic engine-empty notification plus the track that raised it.
    /// This is durable across polling intervals; a boolean pulse would have
    /// the same sampling race as the old `was_playing` predicate.
    engine_empty_generation: Arc<AtomicU64>,
    engine_empty_track_id: Arc<AtomicU64>,
    /// Monotonic terminal source-data failure notification. Kept separate
    /// from `stream_error`, which also represents output-device failures that
    /// must never cause the queue to skip a healthy track.
    source_failure_generation: Arc<AtomicU64>,
    source_failure_track_id: Arc<AtomicU64>,
    /// Handle of the live CMAF segment feeder — the background task that
    /// keeps downloading the currently-streaming track after playback has
    /// started. Stored so a superseding play intent can stop the download
    /// the user just abandoned (`cancel_stream_feeder`) and so the abandoned
    /// buffer can be sealed once the new source is in place
    /// (`seal_stream_feeder`).
    stream_feeder: Arc<std::sync::Mutex<Option<StreamFeederSlot>>>,
    /// Feeder for an incrementally-buffered gapless successor. It remains
    /// separate from `stream_feeder` because both downloads can coexist for
    /// the current track's final seconds. A normal play/stop cancels both;
    /// the actual handoff promotes this slot to current ownership.
    gapless_stream_feeder: Arc<std::sync::Mutex<Option<StreamFeederSlot>>>,
}

/// A registered live segment feeder: which track it feeds, how to cancel
/// its download, and the writer into its playback buffer.
struct StreamFeederSlot {
    track_id: u64,
    cancel_tx: tokio::sync::watch::Sender<bool>,
    writer: BufferWriter,
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            is_playing: Arc::new(AtomicBool::new(false)),
            position: Arc::new(AtomicU64::new(0)),
            duration: Arc::new(AtomicU64::new(0)),
            current_track_id: Arc::new(AtomicU64::new(0)),
            dsd_direct: Arc::new(std::sync::atomic::AtomicU8::new(0)),
            has_loaded_audio: Arc::new(AtomicBool::new(false)),
            volume: Arc::new(AtomicU32::new(0.75f32.to_bits())),
            hardware_volume_active: Arc::new(AtomicBool::new(false)),
            playback_start_millis: Arc::new(AtomicU64::new(0)),
            position_at_start: Arc::new(AtomicU64::new(0)),
            current_device: Arc::new(std::sync::RwLock::new(None)),
            stream_error: Arc::new(AtomicBool::new(false)),
            stream_error_message: Arc::new(std::sync::RwLock::new(None)),
            sample_rate: Arc::new(AtomicU32::new(0)),
            bit_depth: Arc::new(AtomicU32::new(0)),
            normalization_gain: Arc::new(AtomicU32::new(0)),
            gapless_ready: Arc::new(AtomicBool::new(false)),
            gapless_next_track_id: Arc::new(AtomicU64::new(0)),
            buffer_progress: Arc::new(AtomicU32::new(0)),
            buffer_state: Arc::new(std::sync::Mutex::new(PlaybackBufferSlot::default())),
            bit_perfect_mode: Arc::new(AtomicU8::new(0)),
            output_stream_format: Arc::new(AtomicU64::new(0)),
            play_generation: Arc::new(AtomicU64::new(0)),
            engine_empty_generation: Arc::new(AtomicU64::new(0)),
            engine_empty_track_id: Arc::new(AtomicU64::new(0)),
            source_failure_generation: Arc::new(AtomicU64::new(0)),
            source_failure_track_id: Arc::new(AtomicU64::new(0)),
            stream_feeder: Arc::new(std::sync::Mutex::new(None)),
            gapless_stream_feeder: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Clearing (`error = false`) also drops any pending
    /// `stream_error_message`. This is intentional: if init recovers before
    /// the Tauri polling loop drains the message, we'd rather swallow the
    /// toast than surface a notification for a transient failure the user
    /// never perceived. The trade-off is that a fast record→clear→drain
    /// sequence loses the message — accepted because a recovered error is
    /// not a user-actionable event.
    /// 0 = none, 1 = DoP, 2 = native BE, 3 = native LE.
    pub fn set_dsd_mode(&self, mode: u8) {
        self.dsd_direct.store(mode, Ordering::SeqCst);
    }

    pub fn dsd_mode(&self) -> u8 {
        self.dsd_direct.load(Ordering::SeqCst)
    }

    pub fn is_dsd_direct(&self) -> bool {
        self.dsd_direct.load(Ordering::SeqCst) != 0
    }

    pub fn set_stream_error(&self, error: bool) {
        self.stream_error.store(error, Ordering::SeqCst);
        if !error {
            if let Ok(mut m) = self.stream_error_message.write() {
                *m = None;
            }
        }
    }

    pub fn has_stream_error(&self) -> bool {
        self.stream_error.load(Ordering::SeqCst)
    }

    /// Record a user-readable error explanation alongside `stream_error=true`.
    /// The message is drained once via `take_stream_error_message` so the UI
    /// fires the toast exactly once per error.
    pub fn record_stream_error(&self, message: impl Into<String>) {
        self.stream_error.store(true, Ordering::SeqCst);
        self.mark_current_buffer_error();
        if let Ok(mut m) = self.stream_error_message.write() {
            *m = Some(message.into());
        }
    }

    /// Atomically take the pending stream-error message (if any). Returns
    /// `None` when no message is pending or has already been read.
    pub fn take_stream_error_message(&self) -> Option<String> {
        self.stream_error_message
            .write()
            .ok()
            .and_then(|mut m| m.take())
    }

    /// Start a new play intent; returns the generation token for this intent
    /// (see `Player::begin_play`).
    pub(crate) fn begin_play(&self) -> u64 {
        let generation = self.play_generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut slot) = self.buffer_state.lock() {
            *slot = PlaybackBufferSlot {
                state: PlaybackBufferState::Idle,
                track_id: 0,
                play_generation: generation,
            };
        }
        generation
    }

    /// The most recent play generation (the token a play command sent right
    /// now would carry).
    pub(crate) fn current_play_generation(&self) -> u64 {
        self.play_generation.load(Ordering::SeqCst)
    }

    /// True while `gen` is still the newest play intent. The audio thread
    /// uses this to abandon buffer waits for superseded plays (#591).
    pub(crate) fn is_current_play(&self, gen: u64) -> bool {
        self.current_play_generation() == gen
    }

    fn begin_buffering(&self, track_id: u64, play_generation: u64) {
        if !self.is_current_play(play_generation) {
            return;
        }
        if let Ok(mut slot) = self.buffer_state.lock() {
            if self.is_current_play(play_generation) {
                *slot = PlaybackBufferSlot {
                    state: PlaybackBufferState::InitialBuffering,
                    track_id,
                    play_generation,
                };
            }
        }
    }

    fn set_buffer_state_for_play(
        &self,
        buffer_state: PlaybackBufferState,
        track_id: u64,
        play_generation: u64,
    ) {
        if !self.is_current_play(play_generation) {
            return;
        }
        if let Ok(mut slot) = self.buffer_state.lock() {
            if self.is_current_play(play_generation)
                && slot.play_generation == play_generation
                && slot.track_id == track_id
            {
                slot.state = buffer_state;
            }
        }
    }

    fn adopt_buffered_track(
        &self,
        track_id: u64,
        state: PlaybackBufferState,
        play_generation: u64,
    ) {
        if !self.is_current_play(play_generation) {
            return;
        }
        if let Ok(mut slot) = self.buffer_state.lock() {
            if self.is_current_play(play_generation)
                && slot.play_generation == play_generation
            {
                *slot = PlaybackBufferSlot {
                    state,
                    track_id,
                    play_generation,
                };
            }
        }
    }

    fn mark_current_buffer_error(&self) {
        let play_generation = self.current_play_generation();
        if let Ok(mut slot) = self.buffer_state.lock() {
            if slot.play_generation == play_generation && slot.track_id != 0 {
                slot.state = PlaybackBufferState::Error;
            }
        }
    }

    pub fn playback_buffer_state(&self) -> PlaybackBufferState {
        self.playback_buffer_snapshot().0
    }

    pub fn playback_buffer_snapshot(&self) -> (PlaybackBufferState, u64) {
        self.buffer_state
            .lock()
            .map(|slot| (slot.state, slot.track_id))
            .unwrap_or((PlaybackBufferState::Error, 0))
    }

    /// Publish a durable engine-empty edge for frontend/headless pollers.
    /// Store the identity first, then advance the generation so a reader that
    /// observes the new generation also observes its matching track id.
    fn record_engine_empty(&self, track_id: u64) {
        self.engine_empty_track_id.store(track_id, Ordering::SeqCst);
        self.engine_empty_generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Publish a terminal CMAF feeder failure only while its play intent is
    /// still current. A superseded feeder is cancellation, not a bad track.
    fn record_source_failure(&self, track_id: u64, play_gen: u64) {
        if !self.is_current_play(play_gen) {
            return;
        }
        self.set_buffer_state_for_play(PlaybackBufferState::Error, track_id, play_gen);
        self.source_failure_track_id
            .store(track_id, Ordering::SeqCst);
        self.source_failure_generation
            .fetch_add(1, Ordering::SeqCst);
    }

    /// Cancel the in-flight segment feeder's download, if any. Called on
    /// every new play intent (and on stop) via `Player::begin_play`.
    ///
    /// Without this, a superseded track's feeder kept downloading to
    /// completion: on 2026-08-10 a shuffle&play pressed mid-stream measured
    /// 5441 ms for the new track's init-segment fetch (vs 423-760 ms on an
    /// idle link) because the abandoned track's segment loop still held the
    /// link — the init's connection setup and TTFB queued behind ~15 MB of
    /// in-flight segment data.
    ///
    /// The cancellation is cooperative (a watch flag the feeder selects on
    /// between and during segment fetches) and deliberately does NOT error
    /// or complete the abandoned buffer: the outgoing engine keeps playing
    /// what is already buffered until the audio thread installs the new
    /// source, which is exactly the hand-off the pre-cancel code had. The
    /// buffer is sealed by `seal_stream_feeder` once the new source is in.
    pub(crate) fn cancel_stream_feeder(&self) {
        for slot in [&self.stream_feeder, &self.gapless_stream_feeder] {
            let feeder = slot.lock().ok().and_then(|s| {
                s.as_ref()
                    .map(|slot| (slot.track_id, slot.cancel_tx.clone()))
            });
            if let Some((track_id, cancel_tx)) = feeder {
                log::info!(
                    "[CMAF-STREAM] Cancelling feeder for superseded track {}",
                    track_id
                );
                let _ = cancel_tx.send(true);
            }
        }
    }

    /// Seal the abandoned feeder's buffer (`download_complete`) and drop the
    /// slot. Called when a new source has just been handed to the audio
    /// thread (and on stop): a reader that ran the old buffer dry and is
    /// parked on its condvar wakes up and sees EOF instead of blocking
    /// forever behind a feeder that no longer exists. For a reader still
    /// working through the buffered remainder this is a no-op — the engine
    /// has already been swapped by the command just sent.
    pub(crate) fn seal_stream_feeder(&self) {
        for feeder in [&self.stream_feeder, &self.gapless_stream_feeder] {
            let slot = feeder.lock().ok().and_then(|mut s| s.take());
            if let Some(slot) = slot {
                let _ = slot.writer.complete();
            }
        }
    }

    /// Register the feeder spawned for `gen`'s stream. The generation check
    /// runs under the slot lock so a concurrent `begin_play` + cancel cannot
    /// slip between the check and the store: if this intent was already
    /// superseded, the new feeder is cancelled instead of registered.
    pub(crate) fn register_stream_feeder(
        &self,
        track_id: u64,
        cancel_tx: tokio::sync::watch::Sender<bool>,
        writer: BufferWriter,
        gen: u64,
    ) {
        if let Ok(mut s) = self.stream_feeder.lock() {
            if !self.is_current_play(gen) {
                drop(s);
                let _ = cancel_tx.send(true);
                return;
            }
            *s = Some(StreamFeederSlot {
                track_id,
                cancel_tx,
                writer,
            });
        }
    }

    fn register_gapless_stream_feeder(
        &self,
        track_id: u64,
        cancel_tx: tokio::sync::watch::Sender<bool>,
        writer: BufferWriter,
        gen: u64,
    ) -> bool {
        let Ok(mut slot) = self.gapless_stream_feeder.lock() else {
            let _ = cancel_tx.send(true);
            return false;
        };
        if !self.is_current_play(gen) {
            drop(slot);
            let _ = cancel_tx.send(true);
            return false;
        }
        if let Some(previous) = slot.replace(StreamFeederSlot {
            track_id,
            cancel_tx,
            writer,
        }) {
            let _ = previous.cancel_tx.send(true);
            let _ = previous.writer.complete();
        }
        true
    }

    fn cancel_gapless_stream_feeder(&self, track_id: u64) {
        let slot = self
            .gapless_stream_feeder
            .lock()
            .ok()
            .and_then(|mut slot| {
                if slot.as_ref().map(|s| s.track_id) == Some(track_id) {
                    slot.take()
                } else {
                    None
                }
            });
        if let Some(slot) = slot {
            let _ = slot.cancel_tx.send(true);
            let _ = slot.writer.complete();
        }
    }

    fn promote_gapless_stream_feeder(&self, track_id: u64) {
        let pending = self
            .gapless_stream_feeder
            .lock()
            .ok()
            .and_then(|mut slot| {
                if slot.as_ref().map(|s| s.track_id) == Some(track_id) {
                    slot.take()
                } else {
                    None
                }
            });
        let Some(pending) = pending else {
            return;
        };
        if let Ok(mut current) = self.stream_feeder.lock() {
            *current = Some(pending);
        }
    }

    pub fn output_stream_format(&self) -> Option<(u32, u16)> {
        let value = self.output_stream_format.load(Ordering::Acquire);
        let rate = value as u32;
        let channels = (value >> 32) as u16;
        (rate > 0 && channels > 0).then_some((rate, channels))
    }
    fn publish_output_stream_format(&self, stream: Option<&StreamType>) {
        let value = stream
            .map(|stream| {
                let (rate, channels) = stream.output_format();
                u64::from(rate) | (u64::from(channels) << 32)
            })
            .unwrap_or(0);
        self.output_stream_format.store(value, Ordering::Release);
    }

    pub fn set_stream_quality(&self, sample_rate: u32, bit_depth: u32) {
        self.sample_rate.store(sample_rate, Ordering::SeqCst);
        self.bit_depth.store(bit_depth, Ordering::SeqCst);
    }

    /// Set the current bit-perfect mode for the active stream.
    /// Pass None when no stream is active (e.g., after stop).
    pub fn set_bit_perfect_mode(&self, mode: Option<BitPerfectMode>) {
        let code = match mode {
            None => 0,
            Some(BitPerfectMode::Disabled) => 1,
            Some(BitPerfectMode::DirectHardware) => 2,
            Some(BitPerfectMode::PluginFallback) => 3,
        };
        self.bit_perfect_mode.store(code, Ordering::SeqCst);
    }

    /// Get the current bit-perfect mode for the active stream.
    /// Returns None when no stream has been initialized yet.
    pub fn get_bit_perfect_mode(&self) -> Option<BitPerfectMode> {
        match self.bit_perfect_mode.load(Ordering::SeqCst) {
            0 => None,
            1 => Some(BitPerfectMode::Disabled),
            2 => Some(BitPerfectMode::DirectHardware),
            3 => Some(BitPerfectMode::PluginFallback),
            _ => None,
        }
    }

    pub fn get_sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::SeqCst)
    }

    pub fn get_bit_depth(&self) -> u32 {
        self.bit_depth.load(Ordering::SeqCst)
    }

    /// Set the current normalization gain factor.
    /// Stores f32 as u32 bits. Pass None (or 0.0) to indicate no normalization.
    pub fn set_normalization_gain(&self, gain: Option<f32>) {
        let bits = gain.unwrap_or(0.0).to_bits();
        self.normalization_gain.store(bits, Ordering::SeqCst);
    }

    /// Get the current normalization gain factor.
    /// Returns None if normalization is not active (gain is 0.0).
    pub fn get_normalization_gain(&self) -> Option<f32> {
        let bits = self.normalization_gain.load(Ordering::SeqCst);
        let gain = f32::from_bits(bits);
        if gain == 0.0 {
            None
        } else {
            Some(gain)
        }
    }

    /// Set streaming buffer progress (0.0 to 1.0). Pass 0.0 when not streaming.
    pub fn set_buffer_progress(&self, progress: f32) {
        self.buffer_progress
            .store(progress.to_bits(), Ordering::SeqCst);
    }

    /// Get streaming buffer progress (0.0 to 1.0). Returns None if not streaming.
    pub fn get_buffer_progress(&self) -> Option<f32> {
        let bits = self.buffer_progress.load(Ordering::SeqCst);
        let progress = f32::from_bits(bits);
        if progress <= 0.0 || progress >= 1.0 {
            None
        } else {
            Some(progress)
        }
    }

    pub fn set_current_device(&self, device: Option<String>) {
        if let Ok(mut d) = self.current_device.write() {
            *d = device;
        }
    }

    pub fn current_device(&self) -> Option<String> {
        self.current_device.read().ok().and_then(|d| d.clone())
    }

    pub fn set_gapless_ready(&self, ready: bool) {
        self.gapless_ready.store(ready, Ordering::SeqCst);
    }

    pub fn is_gapless_ready(&self) -> bool {
        self.gapless_ready.load(Ordering::SeqCst)
    }

    pub fn set_gapless_next_track_id(&self, track_id: u64) {
        self.gapless_next_track_id.store(track_id, Ordering::SeqCst);
    }

    pub fn get_gapless_next_track_id(&self) -> u64 {
        self.gapless_next_track_id.load(Ordering::SeqCst)
    }

    /// Get current position based on elapsed time since playback started
    pub fn current_position(&self) -> u64 {
        if !self.is_playing.load(Ordering::SeqCst) {
            return self.position.load(Ordering::SeqCst);
        }

        let start_millis = self.playback_start_millis.load(Ordering::SeqCst);
        if start_millis == 0 {
            return self.position.load(Ordering::SeqCst);
        }

        let now_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let elapsed_secs = (now_millis.saturating_sub(start_millis)) / 1000;
        let position_at_start = self.position_at_start.load(Ordering::SeqCst);
        let duration = self.duration.load(Ordering::SeqCst);

        // Clamp to duration — unless it is unknown (0): a decoder that could
        // not derive one must not pin the clock to 0 while audio plays.
        let position = position_at_start + elapsed_secs;
        if duration == 0 {
            position
        } else {
            position.min(duration)
        }
    }

    /// Millisecond-precision companion to [`Self::current_position`] — the
    /// exact same derivation WITHOUT the whole-second truncation. READ-ONLY
    /// state derivation from the existing anchors (`playback_start_millis`
    /// is already epoch-ms; `position_at_start`/`position`/`duration` are
    /// seconds): no stream, seek, format or device path is touched.
    ///
    /// Added for the lyrics sync engine (karaoke needs sub-second
    /// resolution); semantics mirror `current_position` line by line:
    /// paused / no anchor → stored coarse position ×1000; playing →
    /// `position_at_start*1000 + (now_ms - start_millis)`, clamped to
    /// `duration*1000`.
    pub fn current_position_ms(&self) -> u64 {
        if !self.is_playing.load(Ordering::SeqCst) {
            return self.position.load(Ordering::SeqCst).saturating_mul(1000);
        }

        let start_millis = self.playback_start_millis.load(Ordering::SeqCst);
        if start_millis == 0 {
            return self.position.load(Ordering::SeqCst).saturating_mul(1000);
        }

        let now_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let elapsed_ms = now_millis.saturating_sub(start_millis);
        let position_at_start_ms = self
            .position_at_start
            .load(Ordering::SeqCst)
            .saturating_mul(1000);
        let duration_ms = self.duration.load(Ordering::SeqCst).saturating_mul(1000);

        // Clamp to duration (same rule as current_position: unknown = no clamp)
        let position_ms = position_at_start_ms.saturating_add(elapsed_ms);
        if duration_ms == 0 {
            position_ms
        } else {
            position_ms.min(duration_ms)
        }
    }

    /// Mark playback as started/resumed at current position
    fn start_playback_timer(&self, position: u64) {
        let now_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        self.playback_start_millis
            .store(now_millis, Ordering::SeqCst);
        self.position_at_start.store(position, Ordering::SeqCst);
    }

    /// Resuming an already running stream must not rebase its live clock on
    /// the stored pause position (which may still be zero).
    fn resume_playback_timer(&self) {
        if !self.is_playing() {
            self.start_playback_timer(self.position.load(Ordering::SeqCst));
        }
    }

    /// Mark playback as paused, saving current position
    fn pause_playback_timer(&self) {
        let current_pos = self.current_position();
        self.position.store(current_pos, Ordering::SeqCst);
        self.playback_start_millis.store(0, Ordering::SeqCst);
    }

    pub fn is_playing(&self) -> bool {
        self.is_playing.load(Ordering::SeqCst)
    }

    pub fn position(&self) -> u64 {
        self.position.load(Ordering::SeqCst)
    }

    pub fn duration(&self) -> u64 {
        self.duration.load(Ordering::SeqCst)
    }

    pub fn current_track_id(&self) -> u64 {
        self.current_track_id.load(Ordering::SeqCst)
    }

    pub fn set_loaded_audio(&self, loaded: bool) {
        self.has_loaded_audio.store(loaded, Ordering::SeqCst);
    }

    pub fn has_loaded_audio(&self) -> bool {
        self.has_loaded_audio.load(Ordering::SeqCst)
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::SeqCst))
    }

    pub fn set_hardware_volume_active(&self, active: bool) {
        self.hardware_volume_active.store(active, Ordering::SeqCst);
    }

    pub fn hardware_volume_active(&self) -> bool {
        self.hardware_volume_active.load(Ordering::SeqCst)
    }
}

/// Audio player that handles streaming playback
/// Uses a dedicated thread for audio output
pub struct Player {
    /// Channel to send commands to the audio thread
    tx: Sender<AudioCommand>,
    /// Shared state accessible from any thread
    pub state: SharedState,
    /// Audio settings (exclusive mode, DAC passthrough, etc.)
    audio_settings: Arc<Mutex<AudioSettings>>,
    /// Visualizer tap for audio sample capture (optional)
    #[allow(dead_code)]
    visualizer_tap: Option<VisualizerTap>,
    /// Bit-depth diagnostic capture (always available, zero-cost when idle)
    pub diagnostic: AudioDiagnostic,
    /// Two-level playback cache (L1 memory + optional L2 disk). A track is
    /// cached after its first play so replays start instantly.
    audio_cache: Arc<qbz_cache::AudioCache>,
    /// Loudness rows (absolute LUFS + peak + source), shared with the audio
    /// thread (start gain, analyser) and the async prefetch (catalog
    /// ReplayGain, pre-analysis).
    loudness_cache: Arc<LoudnessCache>,
}

impl Default for Player {
    fn default() -> Self {
        Self::new(None, AudioSettings::default(), None, AudioDiagnostic::new())
    }
}

impl Player {
    /// Create a new player with an optional specific output device and audio settings
    /// If device_name is None, uses the system default device
    /// visualizer_tap is optional - if provided, audio samples are captured for visualization
    pub fn new(
        device_name: Option<String>,
        audio_settings: AudioSettings,
        visualizer_tap: Option<VisualizerTap>,
        diagnostic: AudioDiagnostic,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<AudioCommand>();
        let state = SharedState::new();
        let thread_state = state.clone();

        // Clone settings for thread
        let settings = Arc::new(Mutex::new(audio_settings.clone()));
        let thread_settings = settings.clone();

        // Clone visualizer tap and diagnostic for audio thread
        let thread_viz_tap = visualizer_tap.clone();
        let thread_diagnostic = diagnostic.clone();

        // Shared with the audio thread (analyser + start gain) AND with the
        // async prefetch (catalog ReplayGain, pre-analysis). A cache is an
        // accelerator, never a reason to abort: this used to `panic!` on the
        // audio thread, which killed it for the rest of the run (every later
        // command failed with "Failed to send") on a full disk, a read-only
        // profile or a corrupt loudness_cache.db. Degrade instead.
        let loudness_cache: Arc<LoudnessCache> = match LoudnessCache::new() {
            Ok(c) => Arc::new(c),
            Err(e) => {
                log::error!(
                    "Failed to open the loudness cache: {e}. Normalization keeps working; \
                     analyses are not persisted this run."
                );
                match LoudnessCache::in_memory() {
                    Ok(c) => Arc::new(c),
                    Err(e2) => {
                        log::error!(
                            "In-memory loudness cache failed too: {e2}. \
                             Normalization analyses will not be cached at all."
                        );
                        Arc::new(LoudnessCache::disabled())
                    }
                }
            }
        };
        let thread_cache = loudness_cache.clone();

        // Spawn dedicated audio thread
        thread::spawn(move || {
            log::info!("Audio thread starting...");

            // Initialize loudness analysis system
            let (analyzer_tx, analyzer_rx) = mpsc::sync_channel::<AnalyzerMessage>(64);
            let loudness_cache = thread_cache;
            let _analyzer_handle = LoudnessAnalyzer::spawn(analyzer_rx, loudness_cache.clone());
            let analyzer_enabled = Arc::new(AtomicBool::new(false));

            // Helper to wrap source with visualizer tap, normalization, and diagnostic capture
            // Pipeline order (normalization ON):
            //   Diagnostic (raw) → AnalyzerTap → DynamicAmplify → Visualizer
            // Pipeline order (normalization OFF — bit-perfect):
            //   Diagnostic (raw) → dormant/seek-waveform AnalyzerTap → Visualizer
            let wrap_source = |source: Box<dyn Source<Item = f32> + Send>,
                               normalization_gain: Option<f32>,
                               gain_atomic: Option<Arc<AtomicU32>>,
                               analyzer_tx: &SyncSender<AnalyzerMessage>,
                               analyzer_enabled: &Arc<AtomicBool>,
                               waveform_track: Option<AnalyzerWaveformTrack>,
                               waveform_start_frame: u64|
             -> Box<dyn Source<Item = f32> + Send> {
                // Diagnostic tap (innermost — captures raw decoded samples)
                let source: Box<dyn Source<Item = f32> + Send> =
                    Box::new(DiagnosticSource::new(source, thread_diagnostic.clone()));

                if gain_atomic.is_some() {
                    analyzer_enabled.store(true, Ordering::SeqCst);
                }
                // The tap is always present but its disabled path is one
                // relaxed flag check. This lets the waveform setting turn on
                // during a track without rebuilding the audible source.
                let source: Box<dyn Source<Item = f32> + Send> =
                    Box::new(AnalyzerTap::new_with_waveform(
                        source,
                        analyzer_tx.clone(),
                        analyzer_enabled.clone(),
                        waveform_track,
                        waveform_start_frame,
                    ));

                // Normalization: dynamic (Phase 2) > static (Phase 1 fallback) > none (bit-perfect)
                let source: Box<dyn Source<Item = f32> + Send> =
                    if let Some(gain_atomic) = gain_atomic {
                        let initial_gain = normalization_gain.unwrap_or(1.0);
                        log::info!(
                            "Audio thread: dynamic normalization enabled (initial gain {:.4})",
                            initial_gain
                        );
                        Box::new(DynamicAmplify::new(source, gain_atomic, initial_gain))
                    } else if let Some(gain) = normalization_gain {
                        log::info!(
                            "Audio thread: applying static normalization gain factor {:.4}",
                            gain
                        );
                        Box::new(source.amplify(gain))
                    } else {
                        source
                    };

                // Visualizer tap (outermost)
                if let Some(ref tap) = thread_viz_tap {
                    // FFT frequency bins must follow the decoded source rate.
                    // Gapless sources are admitted only when their format
                    // matches the current stream, so preparing the next one
                    // cannot move this clock ahead to a different rate.
                    tap.set_sample_rate(source.sample_rate().get());
                    Box::new(TappedSource::new(
                        source,
                        tap.ring_buffer.clone(),
                        tap.enabled.clone(),
                    ))
                } else {
                    source
                }
            };

            // Get the audio host
            let host = rodio::cpal::default_host();

            // Helper to validate a device has supported output configs
            let is_device_valid = |d: &rodio::cpal::Device| -> bool {
                d.supported_output_configs()
                    .map(|configs| configs.count() > 0)
                    .unwrap_or(false)
            };

            // Helper to find and initialize audio device
            // Try backend system first, fall back to legacy CPAL
            // Takes desired sample_rate and channels to maintain DAC passthrough
            let init_device = |name: &Option<String>,
                               state: &SharedState,
                               sample_rate: u32,
                               channels: u16|
             -> Option<StreamType> {
                // Try backend system if configured
                if let Ok(settings) = thread_settings.lock() {
                    if settings.backend_type.is_some() || cfg!(target_os = "macos") {
                        // Use provided sample rate/channels to maintain DAC passthrough
                        log::info!(
                            "Initializing backend system with {}Hz/{}ch",
                            sample_rate,
                            channels
                        );
                        match try_init_stream_with_backend(&settings, sample_rate, channels, state)
                        {
                            Some(Ok(stream_type)) => {
                                // Set device name from settings for backend system
                                let device_name = settings
                                    .output_device
                                    .clone()
                                    .unwrap_or_else(|| "Default".to_string());
                                log::info!("Audio output initialized via backend system at {}Hz (device: {})", sample_rate, device_name);
                                state.set_current_device(Some(device_name));
                                return Some(stream_type);
                            }
                            Some(Err(e)) => {
                                // On macOS, the backend path is the only one that
                                // understands CoreAudio ownership and nominal-rate
                                // validation. Falling through to legacy CPAL can
                                // create a stream at a stale/source rate; in
                                // shared mode that can produce wrong-speed audio,
                                // and in Exclusive Mode it would silently drop Hog
                                // Mode. Surface the failure instead.
                                #[cfg(target_os = "macos")]
                                if settings
                                    .backend_type
                                    .unwrap_or(AudioBackendType::SystemDefault)
                                    == AudioBackendType::SystemDefault
                                {
                                    log::error!(
                                        "Could not start macOS audio output — {}. Not falling back to the legacy CPAL path because it would either play at the wrong speed (shared mode) or silently drop Exclusive Mode.",
                                        e
                                    );
                                    state.set_current_device(None);
                                    state.record_stream_error(e.clone());
                                    return None;
                                }
                                log::warn!(
                                    "Backend system init failed: {}, falling back to legacy",
                                    e
                                );
                            }
                            None => {
                                // Backend not configured, continue to legacy path
                            }
                        }
                    }
                }

                // Legacy CPAL path
                let device = if let Some(ref name) = name {
                    log::info!("Looking for audio device: {}", name);
                    let found = host.output_devices().ok().and_then(|mut devices| {
                        devices.find(|d| cpal_device_name(d).as_deref() == Some(name.as_str()))
                    });

                    match found {
                        Some(d) if is_device_valid(&d) => {
                            log::info!("Found and validated device: {}", name);
                            Some(d)
                        }
                        Some(_) => {
                            log::warn!(
                                "Device '{}' found but has no valid output configs, using default",
                                name
                            );
                            host.default_output_device()
                        }
                        None => {
                            log::warn!("Device '{}' not found, using default", name);
                            host.default_output_device()
                        }
                    }
                } else {
                    log::info!("Using default audio device");
                    host.default_output_device()
                };

                let device = match device {
                    Some(d) => {
                        if let Some(name) = cpal_device_name(&d) {
                            log::info!("Using audio device: {}", name);
                            state.set_current_device(Some(name));
                        }
                        d
                    }
                    None => {
                        log::error!("No audio output device available");
                        state.set_current_device(None);
                        return None;
                    }
                };

                match DeviceSinkBuilder::from_device(device).and_then(|b| b.open_sink_or_fallback())
                {
                    Ok(mixer_sink) => {
                        log::info!("Audio output initialized successfully");
                        Some(StreamType::rodio(mixer_sink))
                    }
                    Err(e) => {
                        log::error!(
                            "Failed to create audio output on device: {}. Trying default...",
                            e
                        );
                        match DeviceSinkBuilder::open_default_sink() {
                            Ok(mixer_sink) => {
                                log::info!("Fallback to default audio output succeeded");
                                Some(StreamType::rodio(mixer_sink))
                            }
                            Err(e2) => {
                                log::error!("Failed to create default audio output: {}", e2);
                                state.set_current_device(None);
                                None
                            }
                        }
                    }
                }
            };

            // Initialize audio device lazily on first playback to avoid idle CPU usage.
            let mut current_device_name = device_name.clone();
            let mut stream_opt: Option<StreamType> = None;
            let mut current_track_sample_rate: Option<u32> = None;
            let mut current_track_channels: Option<u16> = None;

            #[allow(dead_code)]
            const MAX_INIT_RETRIES: u32 = 5;
            #[allow(dead_code)]
            const RETRY_DELAY_MS: u64 = 500;

            let mut current_engine: Option<PlaybackEngine> = None;
            // Store audio data for seeking (we need to re-decode from the beginning)
            let mut current_audio_data: Option<Arc<Vec<u8>>> = None;
            // Store streaming source for resume (when download completes, we can get the data)
            let mut current_streaming_source: Option<Arc<BufferedMediaSource>> = None;
            // Direct DSD bypasses rodio and therefore has no cached PCM/audio
            // source to rebuild on seek. Keep the container identity instead.
            let mut current_direct_dsd: Option<DirectDsdMedia> = None;
            // Track consecutive sink creation failures to detect broken streams
            let mut consecutive_sink_failures: u32 = 0;
            const MAX_SINK_FAILURES: u32 = 3;
            // Delay dropping the audio stream after pause to reduce CPU usage.
            const PAUSE_SUSPEND_DELAY_MS: u64 = 2000;
            let mut pause_suspend_deadline: Option<Instant> = None;
            let mut last_empty_check = Instant::now();
            // #660: when the last wedged-stream rebuild happened (backoff input).
            let mut last_wedge_rebuild: Option<Instant> = None;
            // Current track's normalization gain factor (stored for reuse on resume/seek)
            let mut current_normalization_gain: Option<f32> = None;
            // Current track's dynamic gain atomic (shared with DynamicAmplify + LoudnessAnalyzer)
            let mut current_gain_atomic: Option<Arc<AtomicU32>> = None;
            // Gapless: pending next track that has been appended to the Sink
            let mut gapless_pending: Option<GaplessPending> = None;
            // Gapless request guard: once we request "next" for a track, do not re-arm
            // until track changes or playback state is reset.
            let mut gapless_request_armed = false;

            log::info!("Audio thread ready and waiting for commands");

            let handle_command =
                |command: AudioCommand,
                 current_engine: &mut Option<PlaybackEngine>,
                 current_audio_data: &mut Option<Arc<Vec<u8>>>,
                 current_streaming_source: &mut Option<Arc<BufferedMediaSource>>,
                 current_direct_dsd: &mut Option<DirectDsdMedia>,
                 stream_opt: &mut Option<StreamType>,
                 current_device_name: &mut Option<String>,
                 consecutive_sink_failures: &mut u32,
                 pause_suspend_deadline: &mut Option<Instant>,
                 current_track_sample_rate: &mut Option<u32>,
                 current_track_channels: &mut Option<u16>,
                 current_normalization_gain: &mut Option<f32>,
                 current_gain_atomic: &mut Option<Arc<AtomicU32>>,
                 gapless_pending: &mut Option<GaplessPending>,
                 gapless_request_armed: &mut bool| {
                    match command {
                        AudioCommand::Play {
                            data,
                            track_id,
                            duration_secs,
                            sample_rate,
                            channels,
                            play_gen,
                        } => {
                            if !thread_state.is_current_play(play_gen) {
                                log::info!(
                                    "Audio thread: cached play of track {} was superseded",
                                    track_id
                                );
                                return;
                            }
                            log::info!(
                                "Audio thread: playing track {} ({}Hz, {} channels)",
                                track_id,
                                sample_rate,
                                channels
                            );
                            *pause_suspend_deadline = None;
                            thread_state.set_dsd_mode(0);
                            *current_direct_dsd = None;
                            // Clear any pending gapless state (new Play supersedes queued gapless)
                            // GAPLESS LIFECYCLE TRACE (2026-08-10): the `X -> X` self-transition
                            // survived the PlayStreaming fix, and guessing at the remaining
                            // path has failed twice. Every set/clear now says who did it.
                            log::info!(
                                "[gapless-trace] clear (Play) pending was {:?}",
                                gapless_pending.as_ref().map(|p| p.track_id)
                            );
                            *gapless_pending = None;
                            *gapless_request_armed = false;
                            thread_state.set_gapless_ready(false);
                            thread_state.set_gapless_next_track_id(0);

                            let StreamRecreateDecision {
                                needs_new_stream,
                                format_changed,
                                dac_passthrough,
                                using_alsa_direct,
                                using_coreaudio_exclusive,
                                coreaudio_shared_rate_mismatch,
                            } = evaluate_stream_recreate(
                                &thread_settings,
                                stream_opt,
                                *current_track_sample_rate,
                                *current_track_channels,
                                sample_rate,
                                channels,
                                "Play",
                            );

                            if needs_new_stream {
                                if stream_opt.is_some() {
                                    // CoreAudio rate-mismatch case already
                                    // logged at warn level by
                                    // `evaluate_stream_recreate`.
                                    if coreaudio_shared_rate_mismatch.is_none()
                                        && (dac_passthrough
                                            || using_alsa_direct
                                            || using_coreaudio_exclusive)
                                        && format_changed
                                    {
                                        let mode = if using_coreaudio_exclusive {
                                            "CoreAudio exclusive"
                                        } else if using_alsa_direct {
                                            "ALSA Direct"
                                        } else {
                                            "DAC passthrough"
                                        };
                                        log::info!(
                                        "Sample rate/channels changed from {:?}Hz/{:?}ch to {}Hz/{}ch - recreating audio stream ({})",
                                        *current_track_sample_rate,
                                        *current_track_channels,
                                        sample_rate,
                                        channels,
                                        mode
                                    );
                                    }
                                    // Stop engine FIRST so its writer thread releases its
                                    // Arc<AlsaDirectStream> reference before we drop the stream.
                                    // Without this, snd_pcm_open() races against the old PCM
                                    // handle and fails with EBUSY.
                                    if let Some(engine) = current_engine.take() {
                                        engine.stop();
                                        std::thread::sleep(Duration::from_millis(50));
                                    }
                                    thread_state.set_hardware_volume_active(false);
                                    // Now this drop is the last Arc ref — PCM actually closes
                                    drop(stream_opt.take());
                                    // Give kernel time to fully release the ALSA device
                                    std::thread::sleep(Duration::from_millis(50));
                                }

                                log::info!(
                                    "DAC passthrough: {}, ALSA Direct: {}, CoreAudio exclusive: {}",
                                    dac_passthrough,
                                    using_alsa_direct,
                                    using_coreaudio_exclusive
                                );

                                // Legacy CPAL path for Play, shared by the
                                // "backend not configured" and "backend init failed"
                                // arms below (#591/#592 follow-up — same shape as the
                                // streaming handler). Uses the track's native
                                // rate/channels unchanged — no resampling is
                                // introduced here.
                                let legacy_stream = || -> Result<StreamType, String> {
                                    // Get the audio device via CPAL
                                    let device = if let Some(ref name) = *current_device_name {
                                        log::info!("Looking for audio device: {}", name);
                                        let found =
                                            host.output_devices().ok().and_then(|mut devices| {
                                                devices.find(|d| {
                                                    cpal_device_name(d).as_deref()
                                                        == Some(name.as_str())
                                                })
                                            });

                                        match found {
                                            Some(d) if is_device_valid(&d) => {
                                                log::info!(
                                                    "Found and validated device: {}",
                                                    name
                                                );
                                                Some(d)
                                            }
                                            Some(_) => {
                                                log::warn!("Device '{}' found but has no valid output configs, using default", name);
                                                host.default_output_device()
                                            }
                                            None => {
                                                log::warn!(
                                                    "Device '{}' not found, using default",
                                                    name
                                                );
                                                host.default_output_device()
                                            }
                                        }
                                    } else {
                                        log::info!("Using default audio device");
                                        host.default_output_device()
                                    };

                                    let Some(device) = device else {
                                        return Err(
                                            "No audio output device available".to_string()
                                        );
                                    };

                                    // Set current device name
                                    if let Some(name) = cpal_device_name(&device) {
                                        log::info!("Using audio device: {}", name);
                                        thread_state.set_current_device(Some(name));
                                    }

                                    create_output_stream_with_config(
                                        device,
                                        sample_rate,
                                        channels,
                                        dac_passthrough,
                                        thread_state.clone(),
                                    )
                                    .map(StreamType::rodio)
                                };

                                // Try backend system first (if configured), then fall back to legacy CPAL
                                // This avoids unnecessary CPAL device enumeration for PipeWire DAC and ALSA Direct
                                let stream_result = if let Some(settings) =
                                    thread_settings.lock().ok()
                                {
                                    match try_init_stream_with_backend(
                                        &settings,
                                        sample_rate,
                                        channels,
                                        &thread_state,
                                    ) {
                                        Some(Ok(stream)) => {
                                            // Backend system handled it - set device name from settings
                                            let device_name = settings
                                                .output_device
                                                .clone()
                                                .unwrap_or_else(|| "Default".to_string());
                                            log::info!(
                                                "Backend system using device: {}",
                                                device_name
                                            );
                                            thread_state.set_current_device(Some(device_name));
                                            Ok(stream)
                                        }
                                        // macOS: the backend path is authoritative
                                        // (CoreAudio ownership + nominal-rate handling);
                                        // surface the failure unchanged — see init_device
                                        // for the rationale.
                                        #[cfg(target_os = "macos")]
                                        Some(Err(e)) => Err(e),
                                        // Linux/other: a backend init failure must not
                                        // dead-end the Play (#591/#592 follow-up). Fall
                                        // back to the legacy CPAL path immediately, same
                                        // as the streaming handler and init_device do.
                                        #[cfg(not(target_os = "macos"))]
                                        Some(Err(e)) => {
                                            log::warn!(
                                                "Backend system init failed for Play: {}, falling back to legacy",
                                                e
                                            );
                                            legacy_stream()
                                        }
                                        None => {
                                            // Backend system not configured, use legacy CPAL path
                                            log::info!("Backend system not configured, using legacy CPAL path");
                                            legacy_stream()
                                        }
                                    }
                                } else {
                                    // Failed to lock settings, use legacy path with CPAL device search
                                    let device = if let Some(ref name) = *current_device_name {
                                        log::info!("Looking for audio device: {}", name);
                                        host.output_devices()
                                            .ok()
                                            .and_then(|mut devices| {
                                                devices.find(|d| {
                                                    cpal_device_name(d).as_deref()
                                                        == Some(name.as_str())
                                                })
                                            })
                                            .or_else(|| {
                                                log::warn!(
                                                    "Device '{}' not found, using default",
                                                    name
                                                );
                                                host.default_output_device()
                                            })
                                    } else {
                                        host.default_output_device()
                                    };

                                    let Some(device) = device else {
                                        log::error!("No audio output device available");
                                        thread_state.set_current_device(None);
                                        thread_state.set_stream_error(true);
                                        return;
                                    };

                                    if let Some(name) = cpal_device_name(&device) {
                                        thread_state.set_current_device(Some(name));
                                    }

                                    create_output_stream_with_config(
                                        device,
                                        sample_rate,
                                        channels,
                                        dac_passthrough,
                                        thread_state.clone(),
                                    )
                                    .map(StreamType::rodio)
                                };

                                // Handle stream creation result
                                match stream_result {
                                    Ok(stream) => {
                                        *stream_opt = Some(stream);
                                        thread_state.set_stream_error(false);

                                        // Set current device name from settings (for backend system)
                                        if let Some(settings) = thread_settings.lock().ok() {
                                            if let Some(ref device_name) = settings.output_device {
                                                thread_state
                                                    .set_current_device(Some(device_name.clone()));
                                                log::info!(
                                                    "Audio stream ready at {}Hz on device: {}",
                                                    sample_rate,
                                                    device_name
                                                );
                                            } else {
                                                thread_state.set_current_device(Some(
                                                    "Default".to_string(),
                                                ));
                                                log::info!(
                                                    "Audio stream ready at {}Hz on default device",
                                                    sample_rate
                                                );
                                            }
                                        } else {
                                            log::info!("Audio stream ready at {}Hz", sample_rate);
                                        }

                                        // Delay to ensure stream is fully initialized before decoder starts
                                        // This prevents sync gaps and allows hardware to stabilize after sample rate changes
                                        // Extra time needed for large sample rate changes (e.g., 88.2kHz → 44.1kHz)
                                        std::thread::sleep(Duration::from_millis(150));
                                    }
                                    Err(e) => {
                                        log::error!(
                                            "❌ Failed to create stream at {}Hz: {}",
                                            sample_rate,
                                            e
                                        );
                                        thread_state.set_stream_error(true);
                                        thread_state.set_current_device(None);
                                        thread_state.set_buffer_state_for_play(
                                            PlaybackBufferState::Error,
                                            track_id,
                                            play_gen,
                                        );
                                        return;
                                    }
                                }
                            } else if format_changed {
                                // Format changed but DAC passthrough is disabled - reuse existing stream
                                log::info!(
                                "Audio format changed from {:?}Hz/{:?}ch to {}Hz/{}ch - reusing audio stream (DAC passthrough disabled, gapless enabled)",
                                *current_track_sample_rate,
                                *current_track_channels,
                                sample_rate,
                                channels
                            );
                            }

                            // Track the decoded source format separately from
                            // the OS output stream rate. In shared mode the
                            // stream can stay at the CoreAudio nominal rate
                            // while each track has its own decoded rate.
                            *current_track_sample_rate = Some(sample_rate);
                            *current_track_channels = Some(channels);

                            let Some(ref stream) = *stream_opt else {
                                log::error!("Audio thread: no audio device available");
                                thread_state.set_buffer_state_for_play(
                                    PlaybackBufferState::Error,
                                    track_id,
                                    play_gen,
                                );
                                return;
                            };

                            // Stop previous engine and wait for sink to release resources.
                            // The 50ms sleep is an ALSA-only workaround for snd_pcm_open()
                            // racing the previous PCM handle's release. On CoreAudio (macOS)
                            // and WASAPI (Windows) the host stream stays open across track
                            // changes, so the sleep just feeds 50ms of silence into the
                            // mixer — audible as a click at each end of the gap.
                            if let Some(engine) = current_engine.take() {
                                engine.stop();
                                #[cfg(target_os = "linux")]
                                std::thread::sleep(Duration::from_millis(50));
                            }
                            thread_state.set_hardware_volume_active(false);

                            *current_audio_data = Some(Arc::new(data.clone()));
                            *current_streaming_source = None; // Clear streaming source for non-streaming playback
                            thread_state.set_loaded_audio(true);

                            // Create PlaybackEngine from StreamType
                            let mut engine = match stream {
                                StreamType::Rodio { sink: mixer_sink, .. } => {
                                    match PlaybackEngine::new_rodio(&mixer_sink.mixer()) {
                                        Ok(e) => {
                                            *consecutive_sink_failures = 0;
                                            thread_state.set_stream_error(false);
                                            e
                                        }
                                        Err(e) => {
                                            *consecutive_sink_failures += 1;
                                            log::error!(
                                                "Failed to create engine (attempt {}): {}",
                                                *consecutive_sink_failures,
                                                e
                                            );

                                            if *consecutive_sink_failures >= MAX_SINK_FAILURES {
                                                log::warn!(
                                                "Audio stream appears broken after {} failures. Auto-reinitializing...",
                                                *consecutive_sink_failures
                                            );
                                                thread_state.set_stream_error(true);

                                                drop(stream_opt.take());
                                                std::thread::sleep(Duration::from_millis(200));

                                                // Use last known sample rate/channels to maintain DAC passthrough
                                                let sr = current_track_sample_rate.unwrap_or(48000);
                                                let ch = current_track_channels.unwrap_or(2);
                                                *stream_opt = init_device(
                                                    current_device_name,
                                                    &thread_state,
                                                    sr,
                                                    ch,
                                                );
                                                if stream_opt.is_some() {
                                                    log::info!("Audio stream auto-reinitialized successfully at {}Hz", sr);
                                                    *consecutive_sink_failures = 0;
                                                    thread_state.set_stream_error(false);
                                                } else {
                                                    log::error!("Auto-reinit failed. Audio device unavailable.");
                                                    thread_state
                                                        .is_playing
                                                        .store(false, Ordering::SeqCst);
                                                    thread_state.set_current_device(None);
                                                }
                                            }
                                            thread_state.set_buffer_state_for_play(
                                                PlaybackBufferState::Error,
                                                track_id,
                                                play_gen,
                                            );
                                            return;
                                        }
                                    }
                                }
                                StreamType::Direct(alsa_stream) => {
                                    *consecutive_sink_failures = 0;
                                    thread_state.set_stream_error(false);
                                    let (hardware_volume, hardware_volume_events) =
                                        direct_hardware_volume_binding(
                                            &thread_settings,
                                            &thread_state,
                                        );
                                    PlaybackEngine::new_direct(
                                        alsa_stream.clone(),
                                        hardware_volume,
                                        thread_viz_tap.clone(),
                                        hardware_volume_events,
                                    )
                                }
                                #[cfg(target_os = "linux")]
                                StreamType::Jack(jack_stream) => {
                                    *consecutive_sink_failures = 0;
                                    thread_state.set_stream_error(false);
                                    PlaybackEngine::new_jack(jack_stream.clone())
                                }
                            };

                            let volume = f32::from_bits(thread_state.volume.load(Ordering::SeqCst));
                            apply_engine_volume(&stream_opt, &engine, volume);

                            let source = match decode_with_fallback(&data) {
                                Ok(s) => s,
                                Err(e) => {
                                    log::error!("Failed to decode audio: {}", e);
                                    thread_state.set_buffer_state_for_play(
                                        PlaybackBufferState::Error,
                                        track_id,
                                        play_gen,
                                    );
                                    return;
                                }
                            };

                            let actual_duration = source
                                .total_duration()
                                .map(|d| d.as_secs())
                                .unwrap_or(duration_secs);
                            thread_state
                                .duration
                                .store(actual_duration, Ordering::SeqCst);

                            // Start gain BEFORE the first sample (normalization.rs):
                            // cache row > ReplayGain tags > unity while measuring.
                            let plan = normalization::plan_start_gain(
                                &thread_settings,
                                &loudness_cache,
                                track_id,
                                Some(&data),
                            );
                            let (normalization, gain_atomic) =
                                (plan.normalization, plan.gain_atomic.clone());

                            *current_normalization_gain = normalization;
                            *current_gain_atomic = gain_atomic.clone();
                            thread_state.set_normalization_gain(normalization);

                            // Wrap source with diagnostic, normalization, and visualizer
                            let source = wrap_source(
                                source,
                                normalization,
                                gain_atomic.clone(),
                                &analyzer_tx,
                                &analyzer_enabled,
                                Some(AnalyzerWaveformTrack {
                                    track_id,
                                    sample_rate,
                                    channels,
                                    duration_secs: actual_duration,
                                    start_frame: 0,
                                    target_lufs: plan.target_lufs,
                                    gain_atomic,
                                    known_gain: plan.known_gain,
                                    prevent_clipping: plan.prevent_clipping,
                                }),
                                0,
                            );
                            if let Err(e) = engine.append(source) {
                                log::error!("Failed to append source to engine: {}", e);
                                thread_state.set_buffer_state_for_play(
                                    PlaybackBufferState::Error,
                                    track_id,
                                    play_gen,
                                );
                                return;
                            }

                            thread_state.set_buffer_state_for_play(
                                PlaybackBufferState::Ready,
                                track_id,
                                play_gen,
                            );

                            thread_state.is_playing.store(true, Ordering::SeqCst);
                            thread_state.position.store(0, Ordering::SeqCst);
                            thread_state
                                .current_track_id
                                .store(track_id, Ordering::SeqCst);
                            thread_state.start_playback_timer(0);

                            *current_engine = Some(engine);
                            log::info!(
                                "Audio thread: playback started, duration: {}s, normalization: {}",
                                actual_duration,
                                normalization
                                    .map(|g| format!("{:.4}x", g))
                                    .unwrap_or_else(|| "off".to_string())
                            );
                        }
                        AudioCommand::PlayStreaming {
                            source,
                            track_id,
                            sample_rate,
                            channels,
                            duration_secs,
                            start_position_secs,
                            content_length,
                            play_gen,
                        } => {
                            log::info!(
                            "Audio thread: starting streaming playback for track {} ({}Hz, {} channels, {}s, start={}s)",
                            track_id,
                            sample_rate,
                            channels,
                            duration_secs,
                            start_position_secs
                        );
                            *pause_suspend_deadline = None;
                            *current_direct_dsd = None;
                            // Clear any pending gapless state — a new
                            // PlayStreaming supersedes queued gapless, exactly
                            // as `AudioCommand::Play` does above.
                            //
                            // THIS ARM WAS THE ONLY START PATH MISSING IT
                            // (Play, PlayDsdDop, PlayDsdNative, Stop and Seek
                            // all clear). The consequences were not local,
                            // because a pending that outlives its engine is
                            // both a liar and a lock:
                            //
                            //  - It LOCKS. The re-arm test below requires
                            //    `gapless_pending.is_none()`, so the stale
                            //    entry silently blocks gapless for the new
                            //    track — no "approaching end" line is ever
                            //    logged for it.
                            //  - It LIES. The transition test is positional
                            //    (`pos >= dur`), not engine-confirmed, so at
                            //    the new track's natural end the stale pending
                            //    fires anyway and swaps `current_track_id` to a
                            //    track whose samples were appended to an engine
                            //    that no longer exists. The engine goes empty
                            //    one tick later.
                            //  - And the swap BLINDS the frontend: its
                            //    end-of-track predicate needs
                            //    `track_id == 0 || track_id == last_track_id`,
                            //    and the id has already moved — so the queue
                            //    never advances and playback stops dead with a
                            //    full queue.
                            //
                            // Observed 2026-08-05: `Gapless transition: track
                            // 371039 -> 269423327` where 269423327 had been
                            // queued SEVEN MINUTES EARLIER against a different
                            // track, and `269423306 -> 269423306`, a pending
                            // that had come to point at itself. Streaming is
                            // the common path for anything not already cached,
                            // which is why this reads as "the playlist only
                            // plays one track".
                            // GAPLESS LIFECYCLE TRACE (2026-08-10): the `X -> X` self-transition
                            // survived the PlayStreaming fix, and guessing at the remaining
                            // path has failed twice. Every set/clear now says who did it.
                            log::info!("[gapless-trace] clear (PlayStreaming) pending was {:?}", gapless_pending.as_ref().map(|p| p.track_id));
                            *gapless_pending = None;
                            *gapless_request_armed = false;
                            thread_state.set_gapless_ready(false);
                            thread_state.set_gapless_next_track_id(0);

                            // Store streaming source for resume capability
                            // When download completes, we can extract the data for resume
                            *current_streaming_source = Some(source.clone());
                            *current_audio_data = None; // Clear regular audio data
                            thread_state.set_loaded_audio(true);

                            // Track metadata is known-good at command time. Store the
                            // duration NOW — current_position() clamps to it, so leaving
                            // it 0 until after the buffer wait freezes the bar at 0:00,
                            // and a Resume that recovers this source after a failed setup
                            // would pin the bar at 0:00 forever (#508 follow-up, 2.0.1
                            // Flatpak report 2026-07-15).
                            thread_state.duration.store(duration_secs, Ordering::SeqCst);

                            let StreamRecreateDecision {
                                needs_new_stream,
                                format_changed,
                                dac_passthrough,
                                using_alsa_direct,
                                using_coreaudio_exclusive,
                                coreaudio_shared_rate_mismatch,
                            } = evaluate_stream_recreate(
                                &thread_settings,
                                stream_opt,
                                *current_track_sample_rate,
                                *current_track_channels,
                                sample_rate,
                                channels,
                                "Streaming",
                            );

                            if needs_new_stream {
                                if stream_opt.is_some() {
                                    // CoreAudio rate-mismatch case already
                                    // logged at warn level by
                                    // `evaluate_stream_recreate`.
                                    if coreaudio_shared_rate_mismatch.is_none()
                                        && (dac_passthrough
                                            || using_alsa_direct
                                            || using_coreaudio_exclusive)
                                        && format_changed
                                    {
                                        let mode = if using_coreaudio_exclusive {
                                            "CoreAudio exclusive"
                                        } else if using_alsa_direct {
                                            "ALSA Direct"
                                        } else {
                                            "DAC passthrough"
                                        };
                                        log::info!(
                                        "Streaming: Sample rate/channels changed to {}Hz/{}ch - recreating audio stream ({})",
                                        sample_rate,
                                        channels,
                                        mode
                                    );
                                    }
                                    // Stop engine FIRST so its writer thread releases its
                                    // Arc<AlsaDirectStream> reference before we drop the stream.
                                    // Without this, snd_pcm_open() races against the old PCM
                                    // handle and fails with EBUSY.
                                    if let Some(engine) = current_engine.take() {
                                        engine.stop();
                                        std::thread::sleep(Duration::from_millis(50));
                                    }
                                    thread_state.set_hardware_volume_active(false);
                                    // Now this drop is the last Arc ref — PCM actually closes
                                    drop(stream_opt.take());
                                    // Give kernel time to fully release the ALSA device
                                    std::thread::sleep(Duration::from_millis(50));
                                }

                                // Legacy CPAL path for streaming, shared by the
                                // "backend not configured" and "backend init failed"
                                // arms below. Uses the track's native rate/channels
                                // unchanged — no resampling is introduced here.
                                let legacy_stream = || -> Result<StreamType, String> {
                                    let device = if let Some(ref name) = *current_device_name {
                                        host.output_devices()
                                            .ok()
                                            .and_then(|mut devices| {
                                                devices.find(|d| {
                                                    cpal_device_name(d).as_deref()
                                                        == Some(name.as_str())
                                                })
                                            })
                                            .or_else(|| host.default_output_device())
                                    } else {
                                        host.default_output_device()
                                    };

                                    let Some(device) = device else {
                                        return Err(
                                            "No audio output device available for streaming"
                                                .to_string(),
                                        );
                                    };

                                    if let Some(name) = cpal_device_name(&device) {
                                        thread_state.set_current_device(Some(name));
                                    }

                                    create_output_stream_with_config(
                                        device,
                                        sample_rate,
                                        channels,
                                        dac_passthrough,
                                        thread_state.clone(),
                                    )
                                    .map(StreamType::rodio)
                                };

                                let stream_result = if let Some(settings) =
                                    thread_settings.lock().ok()
                                {
                                    match try_init_stream_with_backend(
                                        &settings,
                                        sample_rate,
                                        channels,
                                        &thread_state,
                                    ) {
                                        Some(Ok(stream)) => {
                                            // Set device name from settings for backend system
                                            let device_name = settings
                                                .output_device
                                                .clone()
                                                .unwrap_or_else(|| "Default".to_string());
                                            log::info!(
                                                "Streaming backend using device: {}",
                                                device_name
                                            );
                                            thread_state.set_current_device(Some(device_name));
                                            Ok(stream)
                                        }
                                        // macOS: the backend path is authoritative
                                        // (CoreAudio ownership + nominal-rate handling);
                                        // surface the failure unchanged — see init_device
                                        // for the rationale.
                                        #[cfg(target_os = "macos")]
                                        Some(Err(e)) => Err(e),
                                        // Linux/other: a backend init failure must not
                                        // leave the track never-starting (#591/#592 — the
                                        // 45s loading watchdog was the only exit). Fall
                                        // back to the legacy CPAL path immediately, same
                                        // as the Play/Resume paths do via init_device.
                                        #[cfg(not(target_os = "macos"))]
                                        Some(Err(e)) => {
                                            log::warn!(
                                                "Backend system init failed for streaming: {}, falling back to legacy",
                                                e
                                            );
                                            legacy_stream()
                                        }
                                        None => {
                                            log::info!("Backend system not configured, using legacy CPAL path");
                                            legacy_stream()
                                        }
                                    }
                                } else {
                                    let device = host.default_output_device();
                                    let Some(device) = device else {
                                        log::error!(
                                            "No audio output device available for streaming"
                                        );
                                        // Same cleanup as the failure arms below: the
                                        // streaming source was already stored at
                                        // command-accept, and a Resume that finds it
                                        // would replay a track that never started.
                                        *current_streaming_source = None;
                                        *current_audio_data = None;
                                        thread_state.set_loaded_audio(false);
                                        thread_state.is_playing.store(false, Ordering::SeqCst);
                                        thread_state.record_stream_error(
                                            "No audio output device available for streaming",
                                        );
                                        return;
                                    };
                                    create_output_stream_with_config(
                                        device,
                                        sample_rate,
                                        channels,
                                        dac_passthrough,
                                        thread_state.clone(),
                                    )
                                    .map(StreamType::rodio)
                                };

                                match stream_result {
                                    Ok(stream) => {
                                        *stream_opt = Some(stream);
                                        thread_state.set_stream_error(false);
                                        log::info!(
                                            "Streaming audio stream ready at {}Hz",
                                            sample_rate
                                        );
                                        std::thread::sleep(Duration::from_millis(150));
                                    }
                                    Err(e) => {
                                        log::error!(
                                            "❌ Failed to create stream for streaming at {}Hz: {}",
                                            sample_rate,
                                            e
                                        );
                                        // Backend AND legacy CPAL both failed — a real
                                        // error the user must see. Clear the zombie
                                        // streaming state (same cleanup as the sibling
                                        // failure arms below); leaving it set lets a
                                        // later Resume replay the completed download
                                        // with duration still 0, pinning the bar at
                                        // 0:00 forever (#508/#592).
                                        *current_streaming_source = None;
                                        *current_audio_data = None;
                                        thread_state.set_loaded_audio(false);
                                        thread_state.is_playing.store(false, Ordering::SeqCst);
                                        thread_state.record_stream_error(e);
                                        return;
                                    }
                                }
                            }

                            // Keep decoded source format current even when
                            // macOS shared mode reuses the same output stream.
                            *current_track_sample_rate = Some(sample_rate);
                            *current_track_channels = Some(channels);

                            let Some(ref stream) = *stream_opt else {
                                log::error!(
                                    "Audio thread: no audio device available for streaming"
                                );
                                return;
                            };

                            // Stop previous engine. ALSA-only sleep — see Play handler
                            // above for rationale (CoreAudio/WASAPI don't race here and the
                            // 50ms gap is audible as a click).
                            if let Some(engine) = current_engine.take() {
                                engine.stop();
                                #[cfg(target_os = "linux")]
                                std::thread::sleep(Duration::from_millis(50));
                            }
                            thread_state.set_hardware_volume_active(false);

                            // Create PlaybackEngine
                            let mut engine = match stream {
                                StreamType::Rodio { sink: mixer_sink, .. } => {
                                    match PlaybackEngine::new_rodio(&mixer_sink.mixer()) {
                                        Ok(e) => {
                                            *consecutive_sink_failures = 0;
                                            thread_state.set_stream_error(false);
                                            e
                                        }
                                        Err(e) => {
                                            log::error!(
                                                "Failed to create engine for streaming: {}",
                                                e
                                            );
                                            *current_streaming_source = None;
                                            *current_audio_data = None;
                                            thread_state.set_loaded_audio(false);
                                            thread_state
                                                .is_playing
                                                .store(false, Ordering::SeqCst);
                                            thread_state.record_stream_error(
                                                "Failed to create the playback engine for streaming",
                                            );
                                            return;
                                        }
                                    }
                                }
                                StreamType::Direct(alsa_stream) => {
                                    let (hardware_volume, hardware_volume_events) =
                                        direct_hardware_volume_binding(
                                            &thread_settings,
                                            &thread_state,
                                        );
                                    PlaybackEngine::new_direct(
                                        alsa_stream.clone(),
                                        hardware_volume,
                                        thread_viz_tap.clone(),
                                        hardware_volume_events,
                                    )
                                }
                                #[cfg(target_os = "linux")]
                                StreamType::Jack(jack_stream) => {
                                    PlaybackEngine::new_jack(jack_stream.clone())
                                }
                            };

                            let volume = f32::from_bits(thread_state.volume.load(Ordering::SeqCst));
                            apply_engine_volume(&stream_opt, &engine, volume);

                            // Wait for minimum buffer before starting playback.
                            // When start_position_secs > 0 (session resume),
                            // also wait for enough buffer to cover the resume
                            // offset plus an 8s headroom — the eager pre-skip
                            // below decodes-and-discards up to the offset and
                            // needs the bytes available without blocking the
                            // audio device on the first pull.
                            log::info!("Streaming: waiting for initial buffer...");
                            let start_wait = Instant::now();
                            let max_wait = Duration::from_secs(60);

                            let bytes_per_sec_estimate: u64 = if duration_secs > 0
                                && content_length > 0
                            {
                                content_length / duration_secs
                            } else {
                                200_000
                            };
                            let resume_buffer_target: u64 = if start_position_secs > 0 {
                                bytes_per_sec_estimate
                                    .saturating_mul(start_position_secs.saturating_add(8))
                            } else {
                                0
                            };

                            let buffer_sufficient = |src: &Arc<BufferedMediaSource>| -> bool {
                                if !src.has_min_buffer() {
                                    return false;
                                }
                                if resume_buffer_target == 0 {
                                    return true;
                                }
                                (src.buffer_size() as u64) >= resume_buffer_target
                            };

                            while !buffer_sufficient(&source)
                                && source.download_error().is_none()
                                && start_wait.elapsed() < max_wait
                                && thread_state.is_current_play(play_gen)
                            {
                                std::thread::sleep(Duration::from_millis(50));
                            }

                            // A newer play intent superseded this one while we
                            // were waiting (user clicked another track). Bail
                            // out immediately — the newer PlayStreaming command
                            // is queued right behind us — instead of blocking
                            // the audio thread up to 60s on a download that no
                            // longer matters (#591). Not an error: no
                            // stream_error, quiet return (same shape as the
                            // timeout return below).
                            if !thread_state.is_current_play(play_gen) {
                                log::info!(
                                    "Streaming: play of track {} superseded after {}ms buffer wait — abandoning",
                                    track_id,
                                    start_wait.elapsed().as_millis()
                                );
                                return;
                            }

                            if !source.has_min_buffer() {
                                // #595 attributes feeder-death vs plain timeout;
                                // #594 clears the transport state so the UI does
                                // not sit on a phantom "playing" with no engine.
                                let err_msg = match source.download_error() {
                                    Some(err) => {
                                        log::error!(
                                            "Streaming: feeder failed before initial buffer: {}",
                                            err
                                        );
                                        format!("Stream feeder failed before buffering: {err}")
                                    }
                                    None => {
                                        log::error!(
                                            "Streaming: timeout waiting for initial buffer"
                                        );
                                        "Timed out waiting for the stream buffer to fill"
                                            .to_string()
                                    }
                                };
                                *current_streaming_source = None;
                                *current_audio_data = None;
                                thread_state.set_loaded_audio(false);
                                thread_state.is_playing.store(false, Ordering::SeqCst);
                                thread_state.record_stream_error(err_msg);
                                thread_state.set_buffer_state_for_play(
                                    PlaybackBufferState::Error,
                                    track_id,
                                    play_gen,
                                );
                                return;
                            }
                            if resume_buffer_target > 0
                                && (source.buffer_size() as u64) < resume_buffer_target
                            {
                                log::warn!(
                                    "Streaming: timed out waiting for resume buffer (got {} bytes, wanted {}); pre-skip may underrun briefly",
                                    source.buffer_size(),
                                    resume_buffer_target
                                );
                            }

                            let buffer_wait_ms = start_wait.elapsed().as_millis();
                            log::info!(
                            "Streaming: buffer ready in {}ms ({} bytes, target {}), creating incremental decoder...",
                            buffer_wait_ms, source.buffer_size(), resume_buffer_target
                        );

                            // Create incremental streaming source - this starts playback IMMEDIATELY
                            // while continuing to decode/download in background
                            let mut incremental_source =
                                match IncrementalStreamingSource::new_for_play(
                                    source.clone(),
                                    PlaybackBufferReporter::new(
                                        thread_state.clone(),
                                        track_id,
                                        play_gen,
                                    ),
                                ) {
                                    Ok(s) => s,
                                    Err(e) => {
                                        log::error!(
                                            "Failed to create incremental streaming source: {}",
                                            e
                                        );
                                        *current_streaming_source = None;
                                        *current_audio_data = None;
                                        thread_state.set_loaded_audio(false);
                                        thread_state.is_playing.store(false, Ordering::SeqCst);
                                        thread_state.record_stream_error(
                                            "Failed to start the streaming decoder",
                                        );
                                        thread_state.set_buffer_state_for_play(
                                            PlaybackBufferState::Error,
                                            track_id,
                                            play_gen,
                                        );
                                        return;
                                    }
                                };

                            // Verify sample rate/channels match what we expected
                            let actual_sr = incremental_source.get_sample_rate();
                            let actual_ch = incremental_source.get_channels();
                            if actual_sr != sample_rate || actual_ch != channels {
                                log::warn!(
                                "Streaming: detected format {}Hz/{}ch differs from expected {}Hz/{}ch",
                                actual_sr, actual_ch, sample_rate, channels
                            );
                            }

                            // Set duration from track metadata (passed from frontend)
                            // This allows the seekbar to show progress even during streaming
                            thread_state.duration.store(duration_secs, Ordering::SeqCst);

                            // Start gain BEFORE the first sample (normalization.rs):
                            // cache row > ReplayGain tags in the buffered head >
                            // unity while measuring.
                            let buffered = source.get_buffered_data();
                            let plan = normalization::plan_start_gain(
                                &thread_settings,
                                &loudness_cache,
                                track_id,
                                buffered.as_deref(),
                            );
                            let (normalization, gain_atomic) =
                                (plan.normalization, plan.gain_atomic.clone());

                            *current_normalization_gain = normalization;
                            *current_gain_atomic = gain_atomic.clone();
                            thread_state.set_normalization_gain(normalization);

                            let disk_resume = source.is_file_backed() && source.is_complete()
                                && start_position_secs > 0;
                            if disk_resume {
                                if let Err(error) = incremental_source.seek_to(Duration::from_secs(start_position_secs)) {
                                    log::error!("Cached disk resume seek failed: {error}");
                                    thread_state.record_stream_error("Cached disk resume seek failed");
                                    return;
                                }
                            }
                            // Box the incremental source to match the expected type
                            let mut source_to_play: Box<dyn Source<Item = f32> + Send> =
                                Box::new(incremental_source);

                            // Eager pre-skip for session resume. Decode and
                            // discard samples here so the engine's first pull
                            // doesn't have to do the work synchronously, which
                            // would underrun the audio device for multi-second
                            // offsets. The buffer wait above guarantees
                            // there's enough downloaded data to feed this loop.
                            if start_position_secs > 0 && !disk_resume {
                                let target_samples: u64 = (start_position_secs)
                                    .saturating_mul(actual_sr as u64)
                                    .saturating_mul(actual_ch as u64);
                                let skip_start = Instant::now();
                                let mut skipped: u64 = 0;
                                while skipped < target_samples {
                                    if source_to_play.next().is_none() {
                                        log::warn!(
                                            "Resume: source ended before reaching {}s (pre-skipped {} samples)",
                                            start_position_secs,
                                            skipped
                                        );
                                        break;
                                    }
                                    skipped += 1;
                                }
                                log::info!(
                                    "Resume: pre-skipped {} samples ({}s) in {}ms",
                                    skipped,
                                    start_position_secs,
                                    skip_start.elapsed().as_millis()
                                );
                            }

                            // Wrap source with diagnostic, normalization, and visualizer
                            let source_to_play = wrap_source(
                                source_to_play,
                                normalization,
                                gain_atomic.clone(),
                                &analyzer_tx,
                                &analyzer_enabled,
                                Some(AnalyzerWaveformTrack {
                                    track_id,
                                    sample_rate: actual_sr,
                                    channels: actual_ch,
                                    duration_secs,
                                    start_frame: start_position_secs
                                        .saturating_mul(actual_sr as u64),
                                    target_lufs: plan.target_lufs,
                                    gain_atomic,
                                    known_gain: plan.known_gain,
                                    prevent_clipping: plan.prevent_clipping,
                                }),
                                start_position_secs.saturating_mul(actual_sr as u64),
                            );
                            if let Err(e) = engine.append(source_to_play) {
                                log::error!("Failed to append streaming source to engine: {}", e);
                                thread_state.set_buffer_state_for_play(
                                    PlaybackBufferState::Error,
                                    track_id,
                                    play_gen,
                                );
                                return;
                            }

                            thread_state.set_dsd_mode(0);
                            thread_state.is_playing.store(true, Ordering::SeqCst);
                            thread_state
                                .position
                                .store(start_position_secs, Ordering::SeqCst);
                            thread_state
                                .current_track_id
                                .store(track_id, Ordering::SeqCst);
                            thread_state.start_playback_timer(start_position_secs);

                            *current_engine = Some(engine);
                            log::info!(
                            "Audio thread: streaming playback STARTED in {}ms at {}s (incremental decode active)",
                            start_wait.elapsed().as_millis(),
                            start_position_secs
                        );
                        }
                        AudioCommand::PlayDsdDop { path, track_id } => {
                            #[cfg(target_os = "linux")]
                            {
                                log::info!(
                                    "Audio thread: DoP playback for track {} ({})",
                                    track_id,
                                    path.display()
                                );
                                *pause_suspend_deadline = None;
                                *current_direct_dsd = None;
                                // GAPLESS LIFECYCLE TRACE (2026-08-10): the `X -> X` self-transition
                                // survived the PlayStreaming fix, and guessing at the remaining
                                // path has failed twice. Every set/clear now says who did it.
                                log::info!(
                                    "[gapless-trace] clear (PlayDsdDop) pending was {:?}",
                                    gapless_pending.as_ref().map(|p| p.track_id)
                                );
                                *gapless_pending = None;
                                *gapless_request_armed = false;
                                thread_state.set_gapless_ready(false);
                                thread_state.set_gapless_next_track_id(0);

                                let demux = match qbz_dsd::open_dsd(&path) {
                                    Ok(d) => d,
                                    Err(e) => {
                                        log::error!("DoP: cannot open DSD file: {}", e);
                                        thread_state.set_stream_error(true);
                                        return;
                                    }
                                };
                                let dop = match qbz_dsd::DopStream::new(demux) {
                                    Ok(d) => d,
                                    Err(e) => {
                                        log::error!("DoP: cannot build DoP stream: {}", e);
                                        thread_state.set_stream_error(true);
                                        return;
                                    }
                                };
                                let carrier = dop.carrier_rate();
                                let dsd_rate = dop.dsd_rate();
                                let duration =
                                    dop.total_frames() / (carrier.max(1) as u64);

                                // DoP always needs a fresh S32 stream at the
                                // carrier rate — tear down whatever is open.
                                if let Some(engine) = current_engine.take() {
                                    engine.stop();
                                    std::thread::sleep(Duration::from_millis(50));
                                }
                                thread_state.set_hardware_volume_active(false);
                                drop(stream_opt.take());
                                std::thread::sleep(Duration::from_millis(50));

                                let device = thread_settings
                                    .lock()
                                    .ok()
                                    .and_then(|s| s.output_device.clone());
                                let Some(device) = device else {
                                    log::error!("DoP: no output device configured");
                                    thread_state.set_stream_error(true);
                                    return;
                                };
                                let stream = match qbz_audio::alsa_backend::create_dop_stream(
                                    &device, carrier, 2,
                                ) {
                                    Ok(st) => Arc::new(st),
                                    Err(e) => {
                                        log::error!("DoP: stream open failed: {}", e);
                                        thread_state.set_stream_error(true);
                                        thread_state.set_current_device(None);
                                        return;
                                    }
                                };
                                log::info!(
                                    "DoP: {} locked at {} Hz carrier on {}",
                                    qbz_dsd::dsd_label(dsd_rate),
                                    carrier,
                                    device
                                );
                                *stream_opt = Some(StreamType::Direct(stream.clone()));
                                thread_state.set_current_device(Some(device));
                                *current_track_sample_rate = Some(carrier);
                                *current_track_channels = Some(2);
                                *current_audio_data = None;
                                *current_streaming_source = None;

                                let mut engine = PlaybackEngine::new_alsa_dop(stream, false);
                                if let Err(e) = engine.append_dop(Box::new(DsdErrorReport::new(
                                    dop,
                                    thread_state.clone(),
                                ))) {
                                    log::error!("DoP: append failed: {}", e);
                                    thread_state.set_stream_error(true);
                                    return;
                                }
                                *current_engine = Some(engine);
                                thread_state.set_loaded_audio(true);
                                thread_state.set_stream_error(false);
                                thread_state.set_stream_quality(dsd_rate, 1);
                                thread_state.duration.store(duration, Ordering::SeqCst);
                                thread_state.set_dsd_mode(1);
                                *current_direct_dsd = Some(DirectDsdMedia {
                                    path: path.clone(),
                                    mode: 1,
                                });
                                thread_state.is_playing.store(true, Ordering::SeqCst);
                                thread_state.position.store(0, Ordering::SeqCst);
                                thread_state
                                    .current_track_id
                                    .store(track_id, Ordering::SeqCst);
                                thread_state.start_playback_timer(0);
                                log::info!("Audio thread: DoP playback STARTED");
                            }
                            #[cfg(not(target_os = "linux"))]
                            {
                                let _ = (path, track_id);
                                log::error!("DoP playback is Linux-only");
                                thread_state.set_stream_error(true);
                            }
                        }
                        AudioCommand::PlayDsdNative { path, track_id } => {
                            #[cfg(target_os = "linux")]
                            {
                                log::info!(
                                    "Audio thread: native DSD playback for track {} ({})",
                                    track_id,
                                    path.display()
                                );
                                *pause_suspend_deadline = None;
                                *current_direct_dsd = None;
                                // GAPLESS LIFECYCLE TRACE (2026-08-10): the `X -> X` self-transition
                                // survived the PlayStreaming fix, and guessing at the remaining
                                // path has failed twice. Every set/clear now says who did it.
                                log::info!(
                                    "[gapless-trace] clear (PlayDsdNative) pending was {:?}",
                                    gapless_pending.as_ref().map(|p| p.track_id)
                                );
                                *gapless_pending = None;
                                *gapless_request_armed = false;
                                thread_state.set_gapless_ready(false);
                                thread_state.set_gapless_next_track_id(0);

                                let info = match qbz_dsd::open_dsd(&path) {
                                    Ok(d) => d.info().clone(),
                                    Err(e) => {
                                        log::error!("Native DSD: cannot open file: {}", e);
                                        thread_state.set_stream_error(true);
                                        return;
                                    }
                                };
                                if info.channels != 2 {
                                    log::error!("Native DSD: stereo only");
                                    thread_state.set_stream_error(true);
                                    return;
                                }
                                let rate = qbz_dsd::native_u32_rate(info.dsd_rate);
                                let duration = (info.sample_count / 32) / (rate.max(1) as u64);

                                if let Some(engine) = current_engine.take() {
                                    engine.stop();
                                    std::thread::sleep(Duration::from_millis(50));
                                }
                                thread_state.set_hardware_volume_active(false);
                                drop(stream_opt.take());
                                std::thread::sleep(Duration::from_millis(50));

                                let device = thread_settings
                                    .lock()
                                    .ok()
                                    .and_then(|s| s.output_device.clone());
                                let Some(device) = device else {
                                    log::error!("Native DSD: no output device configured");
                                    thread_state.set_stream_error(true);
                                    return;
                                };
                                let (stream, little_endian) =
                                    match qbz_audio::alsa_backend::create_native_dsd_stream(
                                        &device,
                                        info.dsd_rate,
                                        2,
                                    ) {
                                        Ok(pair) => pair,
                                        Err(e) => {
                                            log::error!("Native DSD: stream open failed: {}", e);
                                            thread_state.set_stream_error(true);
                                            thread_state.set_current_device(None);
                                            return;
                                        }
                                    };
                                let stream = Arc::new(stream);
                                let native_src = match qbz_dsd::open_dsd(&path)
                                    .map_err(|e| e.to_string())
                                    .and_then(|d| {
                                        qbz_dsd::NativeDsdStream::new(d, little_endian)
                                            .map_err(|e| e.to_string())
                                    }) {
                                    Ok(n) => n,
                                    Err(e) => {
                                        log::error!("Native DSD: source build failed: {}", e);
                                        thread_state.set_stream_error(true);
                                        return;
                                    }
                                };
                                log::info!(
                                    "Native DSD: {} locked at {} Hz U32 ({}) on {}",
                                    qbz_dsd::dsd_label(info.dsd_rate),
                                    rate,
                                    if little_endian { "LE" } else { "BE" },
                                    device
                                );
                                *stream_opt = Some(StreamType::Direct(stream.clone()));
                                thread_state.set_current_device(Some(device));
                                *current_track_sample_rate = Some(rate);
                                *current_track_channels = Some(2);
                                *current_audio_data = None;
                                *current_streaming_source = None;

                                let mut engine = PlaybackEngine::new_alsa_dop(stream, true);
                                if let Err(e) = engine.append_dop(Box::new(DsdErrorReport::new(
                                    native_src,
                                    thread_state.clone(),
                                ))) {
                                    log::error!("Native DSD: append failed: {}", e);
                                    thread_state.set_stream_error(true);
                                    return;
                                }
                                *current_engine = Some(engine);
                                thread_state.set_loaded_audio(true);
                                thread_state.set_stream_error(false);
                                thread_state.set_stream_quality(info.dsd_rate, 1);
                                thread_state.duration.store(duration, Ordering::SeqCst);
                                let direct_mode = if little_endian { 3 } else { 2 };
                                thread_state.set_dsd_mode(direct_mode);
                                *current_direct_dsd = Some(DirectDsdMedia {
                                    path: path.clone(),
                                    mode: direct_mode,
                                });
                                thread_state.is_playing.store(true, Ordering::SeqCst);
                                thread_state.position.store(0, Ordering::SeqCst);
                                thread_state
                                    .current_track_id
                                    .store(track_id, Ordering::SeqCst);
                                thread_state.start_playback_timer(0);
                                log::info!("Audio thread: native DSD playback STARTED");
                            }
                            #[cfg(not(target_os = "linux"))]
                            {
                                let _ = (path, track_id);
                                log::error!("Native DSD playback is Linux-only");
                                thread_state.set_stream_error(true);
                            }
                        }
                        AudioCommand::PlayNextDsdDop { path, track_id } => {
                            #[cfg(target_os = "linux")]
                            {
                                let Some(engine) = current_engine.as_mut() else {
                                    thread_state.set_gapless_ready(false);
                                    return;
                                };
                                if !engine.is_dop() {
                                    log::info!("Gapless DoP: engine is not DoP, ignoring");
                                    thread_state.set_gapless_ready(false);
                                    return;
                                }
                                // Build the packing matching the ACTIVE
                                // direct mode (1 = DoP, 2/3 = native BE/LE).
                                let mode = thread_state.dsd_mode();
                                let built: Result<
                                    (Box<dyn Iterator<Item = i32> + Send>, u32, u64),
                                    String,
                                > = qbz_dsd::open_dsd(&path)
                                    .map_err(|e| e.to_string())
                                    .and_then(|d| match mode {
                                        1 => qbz_dsd::DopStream::new(d)
                                            .map_err(|e| e.to_string())
                                            .map(|st| {
                                                let rate = st.carrier_rate();
                                                let frames = st.total_frames();
                                                (
                                                    Box::new(DsdErrorReport::new(
                                                        st,
                                                        thread_state.clone(),
                                                    ))
                                                        as Box<
                                                            dyn Iterator<Item = i32> + Send,
                                                        >,
                                                    rate,
                                                    frames,
                                                )
                                            }),
                                        2 | 3 => qbz_dsd::NativeDsdStream::new(d, mode == 3)
                                            .map_err(|e| e.to_string())
                                            .map(|st| {
                                                let rate = st.rate();
                                                let frames = st.total_frames();
                                                (
                                                    Box::new(DsdErrorReport::new(
                                                        st,
                                                        thread_state.clone(),
                                                    ))
                                                        as Box<
                                                            dyn Iterator<Item = i32> + Send,
                                                        >,
                                                    rate,
                                                    frames,
                                                )
                                            }),
                                        _ => Err("no DSD-direct mode active".to_string()),
                                    });
                                let (src, rate, total_frames) = match built {
                                    Ok(v) => v,
                                    Err(e) => {
                                        log::warn!("Gapless DSD: cannot open next track: {}", e);
                                        thread_state.set_gapless_ready(false);
                                        return;
                                    }
                                };
                                if *current_track_sample_rate != Some(rate) {
                                    log::info!(
                                        "Gapless DSD: rate mismatch ({:?} vs {}), ignoring",
                                        *current_track_sample_rate,
                                        rate
                                    );
                                    thread_state.set_gapless_ready(false);
                                    return;
                                }
                                let duration = total_frames / (rate.max(1) as u64);
                                match engine.append_dop(src) {
                                    Ok(()) => {
                                        // data stays empty: the DoP engine never
                                        // resumes from current_audio_data (the
                                        // pause-suspend teardown is gated off in
                                        // DoP mode).
                                        log::info!("[gapless-trace] set (dop) track {track_id}");
                                        *gapless_pending = Some(GaplessPending {
                                            track_id,
                                            play_generation: thread_state.current_play_generation(),
                                            duration_secs: duration,
                                            media: GaplessMedia::Direct(DirectDsdMedia {
                                                path: path.clone(),
                                                mode,
                                            }),
                                            sample_rate: rate,
                                            channels: 2,
                                            bit_depth: 1,
                                            normalization_gain: None,
                                            gain_atomic: None,
                                        });
                                        thread_state.set_gapless_next_track_id(track_id);
                                        thread_state.set_gapless_ready(false);
                                        log::info!(
                                            "Gapless DoP: queued track {} for seamless DSD transition",
                                            track_id
                                        );
                                    }
                                    Err(e) => {
                                        log::warn!("Gapless DoP: append failed: {}", e);
                                        thread_state.set_gapless_ready(false);
                                    }
                                }
                            }
                            #[cfg(not(target_os = "linux"))]
                            {
                                let _ = (path, track_id);
                                thread_state.set_gapless_ready(false);
                            }
                        }
                        AudioCommand::Pause => {
                            if let Some(ref engine) = *current_engine {
                                engine.pause();
                                thread_state.pause_playback_timer();
                                thread_state.is_playing.store(false, Ordering::SeqCst);
                                *pause_suspend_deadline = Some(
                                    Instant::now() + Duration::from_millis(PAUSE_SUSPEND_DELAY_MS),
                                );
                                log::info!(
                                    "Audio thread: paused at {}s",
                                    thread_state.position.load(Ordering::SeqCst)
                                );
                            }
                        }
                        AudioCommand::Resume => {
                            *pause_suspend_deadline = None;
                            if current_engine.is_none() {
                                // Try to get audio data from regular storage or streaming source
                                let disk_source = current_streaming_source.as_ref()
                                    .filter(|source| source.is_complete() && source.is_file_backed())
                                    .cloned();
                                let audio_data: Arc<Vec<u8>> = if disk_source.is_some() {
                                    Arc::new(Vec::new())
                                } else if let Some(ref data) =
                                    *current_audio_data
                                {
                                    data.clone()
                                } else if let Some(ref streaming_src) = *current_streaming_source {
                                    // Try to get complete data from streaming source
                                    if streaming_src.is_complete() {
                                        match streaming_src.complete_data_shared() {
                                            Some(data) => {
                                                log::info!("Resume: using complete streaming data ({} bytes)", data.len());
                                                // Store it in current_audio_data for future use
                                                *current_audio_data = Some(data.clone());
                                                data
                                            }
                                            None => {
                                                log::warn!("Audio thread: cannot resume - streaming source complete but data unavailable");
                                                return;
                                            }
                                        }
                                    } else {
                                        log::warn!("Audio thread: cannot resume - streaming not complete yet ({} bytes buffered)",
                                        streaming_src.buffer_size());
                                        return;
                                    }
                                } else {
                                    log::warn!(
                                        "Audio thread: cannot resume - no audio data available"
                                    );
                                    return;
                                };

                                if stream_opt.is_none() {
                                    // Use last known sample rate/channels to maintain DAC passthrough
                                    let sr = current_track_sample_rate.unwrap_or(48000);
                                    let ch = current_track_channels.unwrap_or(2);
                                    log::info!(
                                        "Resume: reinitializing stream at {}Hz/{}ch",
                                        sr,
                                        ch
                                    );
                                    *stream_opt =
                                        init_device(current_device_name, &thread_state, sr, ch);
                                }

                                let Some(ref stream) = *stream_opt else {
                                    log::error!(
                                        "Audio thread: cannot resume - no audio device available"
                                    );
                                    return;
                                };

                                let mut engine = match stream {
                                    StreamType::Rodio { sink: mixer_sink, .. } => {
                                        match PlaybackEngine::new_rodio(&mixer_sink.mixer()) {
                                            Ok(e) => e,
                                            Err(e) => {
                                                log::error!(
                                                    "Failed to create engine for resume: {}",
                                                    e
                                                );
                                                return;
                                            }
                                        }
                                    }
                                    StreamType::Direct(alsa_stream) => {
                                        let (hardware_volume, hardware_volume_events) =
                                            direct_hardware_volume_binding(
                                                &thread_settings,
                                                &thread_state,
                                            );
                                        PlaybackEngine::new_direct(
                                            alsa_stream.clone(),
                                            hardware_volume,
                                            thread_viz_tap.clone(),
                                            hardware_volume_events,
                                        )
                                    }
                                    #[cfg(target_os = "linux")]
                                    StreamType::Jack(jack_stream) => {
                                        PlaybackEngine::new_jack(jack_stream.clone())
                                    }
                                };

                                let volume =
                                    f32::from_bits(thread_state.volume.load(Ordering::SeqCst));
                                apply_engine_volume(&stream_opt, &engine, volume);

                                let resume_pos = thread_state.position.load(Ordering::SeqCst);
                                let source: Result<Box<dyn Source<Item = f32> + Send>, String> =
                                    if let Some(disk) = disk_source.as_ref() {
                                        IncrementalStreamingSource::new(disk.clone()).and_then(|mut source| {
                                            if resume_pos > 0 { source.seek_to(Duration::from_secs(resume_pos))?; }
                                            Ok(Box::new(source) as _)
                                        })
                                    } else {
                                        decode_with_fallback(&audio_data)
                                    };
                                let source = match source {
                                    Ok(source) => source,
                                    Err(error) => { log::error!("Failed to decode audio for resume: {error}"); return; }
                                };

                                // A Resume that rebuilds from completed streaming data
                                // can run after a PlayStreaming that failed BEFORE
                                // storing the duration. Backfill from the decoded
                                // source so the position clamp (current_position)
                                // doesn't pin the bar at 0:00 (#508). Same derivation
                                // the Play handler uses. Must read total_duration()
                                // BEFORE skip_duration consumes `source` below.
                                if thread_state.duration.load(Ordering::SeqCst) == 0 {
                                    if let Some(d) = source.total_duration() {
                                        thread_state
                                            .duration
                                            .store(d.as_secs(), Ordering::SeqCst);
                                    }
                                }

                                let skipped_source: Box<dyn Source<Item = f32> + Send> =
                                    if resume_pos > 0 && disk_source.is_none() {
                                        Box::new(
                                            source.skip_duration(Duration::from_secs(resume_pos)),
                                        )
                                    } else {
                                        source
                                    };

                                // Wrap source with diagnostic, normalization, and visualizer
                                // Reuse the gain + atomic from the original Play
                                let skipped_source = wrap_source(
                                    skipped_source,
                                    *current_normalization_gain,
                                    current_gain_atomic.clone(),
                                    &analyzer_tx,
                                    &analyzer_enabled,
                                    None,
                                    resume_pos.saturating_mul(
                                        (*current_track_sample_rate).unwrap_or(0) as u64,
                                    ),
                                );
                                if let Err(e) = engine.append(skipped_source) {
                                    log::error!("Failed to append source for resume: {}", e);
                                    return;
                                }
                                thread_state.start_playback_timer(resume_pos);
                                thread_state.is_playing.store(true, Ordering::SeqCst);
                                *current_engine = Some(engine);

                                log::info!("Audio thread: resumed from {}s", resume_pos);
                                return;
                            }

                            if let Some(ref engine) = *current_engine {
                                engine.play();
                                thread_state.resume_playback_timer();
                                thread_state.is_playing.store(true, Ordering::SeqCst);
                                log::info!("Audio thread: resumed");
                            }
                        }
                        AudioCommand::Stop { completed } => {
                            if let Some(engine) = current_engine.take() {
                                engine.stop();
                            }
                            thread_state.set_hardware_volume_active(false);
                            *current_audio_data = None;
                            *current_streaming_source = None;
                            *current_direct_dsd = None;
                            *current_normalization_gain = None;
                            *current_gain_atomic = None;
                            // GAPLESS LIFECYCLE TRACE (2026-08-10): the `X -> X` self-transition
                            // survived the PlayStreaming fix, and guessing at the remaining
                            // path has failed twice. Every set/clear now says who did it.
                            log::info!(
                                "[gapless-trace] clear (Stop) pending was {:?}",
                                gapless_pending.as_ref().map(|p| p.track_id)
                            );
                            *gapless_pending = None;
                            *gapless_request_armed = false;
                            thread_state.set_gapless_ready(false);
                            thread_state.set_gapless_next_track_id(0);
                            analyzer_enabled.store(false, Ordering::SeqCst);
                            thread_state.set_normalization_gain(None);
                            thread_state.is_playing.store(false, Ordering::SeqCst);
                            thread_state.position.store(0, Ordering::SeqCst);
                            thread_state.set_loaded_audio(false);
                            thread_state
                                .playback_start_millis
                                .store(0, Ordering::SeqCst);
                            thread_state.position_at_start.store(0, Ordering::SeqCst);
                            // Defer dropping the stream so a Play immediately following Stop
                            // (the frontend's track-change pattern is Stop → Play, not append)
                            // can reuse the open device. Tearing CoreAudio down between every
                            // track was producing the audible click on track change. The idle
                            // loop's pause-suspend handler (below) drops the stream when this
                            // deadline fires; Play / Resume / Seek / ReinitDevice all clear
                            // the deadline so they reuse or replace the stream as needed.
                            *pause_suspend_deadline = Some(
                                Instant::now() + Duration::from_millis(PAUSE_SUSPEND_DELAY_MS),
                            );
                            // Reset the PipeWire clock if WE forced it. The call is
                            // self-gating (reset_pipewire_clock no-ops unless QBZ set
                            // the force), so it is now unconditional: the previous
                            // pw_force_bitperfect gate missed plain (no-passthrough)
                            // PipeWire users and leaked a forced rate after stop (#263).
                            #[cfg(target_os = "linux")]
                            qbz_audio::pipewire_backend::PipeWireBackend::reset_pipewire_clock();
                            log::info!("Audio thread: stopped");
                            if let Some(completed) = completed {
                                let _ = completed.send(());
                            }
                        }
                        AudioCommand::SetVolume(volume) => {
                            let reported_volume = reported_volume_after_command(
                                volume,
                                matches!(stream_opt.as_ref(), Some(StreamType::Direct(_))),
                                thread_state.hardware_volume_active(),
                            );
                            thread_state
                                .volume
                                .store(reported_volume.to_bits(), Ordering::SeqCst);
                            if let Some(ref engine) = *current_engine {
                                apply_engine_volume(&stream_opt, &engine, volume);
                            }
                            // debug: a slider drag delivers dozens of these per
                            // second — at info they dominated a field log (#555,
                            // 758 of 1025 lines) and each one is a formatted
                            // write from the AUDIO thread.
                            log::debug!("Audio thread: volume set to {}", volume);
                        }
                        AudioCommand::Seek(position_secs) => {
                            *pause_suspend_deadline = None;
                            // Seeking invalidates a queued successor in every
                            // engine, including direct DSD where the live
                            // source is replaced in place.
                            // GAPLESS LIFECYCLE TRACE (2026-08-10): the `X -> X` self-transition
                            // survived the PlayStreaming fix, and guessing at the remaining
                            // path has failed twice. Every set/clear now says who did it.
                            log::info!(
                                "[gapless-trace] clear (Seek) pending was {:?}",
                                gapless_pending.as_ref().map(|p| p.track_id)
                            );
                            *gapless_pending = None;
                            *gapless_request_armed = false;
                            thread_state.set_gapless_ready(false);
                            thread_state.set_gapless_next_track_id(0);

                            #[cfg(target_os = "linux")]
                            if current_engine.as_ref().map(|e| e.is_dop()).unwrap_or(false) {
                                let Some(media) = current_direct_dsd.as_ref().cloned() else {
                                    log::error!(
                                        "Direct DSD seek aborted: current container is unknown"
                                    );
                                    thread_state.set_stream_error(true);
                                    return;
                                };
                                let target_secs = position_secs.min(thread_state.duration());
                                let mut demux = match qbz_dsd::open_dsd(&media.path) {
                                    Ok(d) => d,
                                    Err(e) => {
                                        log::error!("Direct DSD seek: cannot reopen source: {e}");
                                        thread_state.set_stream_error(true);
                                        return;
                                    }
                                };
                                let dsd_rate = demux.info().dsd_rate;
                                if let Err(e) =
                                    demux.seek_to_bit(target_secs.saturating_mul(dsd_rate as u64))
                                {
                                    log::error!("Direct DSD seek: demux seek failed: {e}");
                                    thread_state.set_stream_error(true);
                                    return;
                                }
                                let built: Result<
                                    (Box<dyn Iterator<Item = i32> + Send>, u32),
                                    String,
                                > = match media.mode {
                                    1 => qbz_dsd::DopStream::new(demux)
                                        .map_err(|e| e.to_string())
                                        .map(|source| {
                                            let rate = source.carrier_rate();
                                            (
                                                Box::new(DsdErrorReport::new(
                                                    source,
                                                    thread_state.clone(),
                                                ))
                                                    as Box<dyn Iterator<Item = i32> + Send>,
                                                rate,
                                            )
                                        }),
                                    2 | 3 => qbz_dsd::NativeDsdStream::new(demux, media.mode == 3)
                                        .map_err(|e| e.to_string())
                                        .map(|source| {
                                            let rate = source.rate();
                                            (
                                                Box::new(DsdErrorReport::new(
                                                    source,
                                                    thread_state.clone(),
                                                ))
                                                    as Box<dyn Iterator<Item = i32> + Send>,
                                                rate,
                                            )
                                        }),
                                    _ => Err(format!("unknown direct DSD mode {}", media.mode)),
                                };
                                let (source, output_rate) = match built {
                                    Ok(v) => v,
                                    Err(e) => {
                                        log::error!("Direct DSD seek: source build failed: {e}");
                                        thread_state.set_stream_error(true);
                                        return;
                                    }
                                };
                                if *current_track_sample_rate != Some(output_rate) {
                                    log::error!(
                                        "Direct DSD seek: active rate {:?} differs from source rate {}",
                                        *current_track_sample_rate,
                                        output_rate
                                    );
                                    thread_state.set_stream_error(true);
                                    return;
                                }
                                let Some(engine) = current_engine.as_mut() else {
                                    return;
                                };
                                if let Err(e) = engine.replace_dop(
                                    source,
                                    target_secs.saturating_mul(output_rate as u64),
                                ) {
                                    log::error!("Direct DSD seek: replace failed: {e}");
                                    thread_state.set_stream_error(true);
                                    return;
                                }

                                let was_playing = thread_state.is_playing.load(Ordering::SeqCst);
                                thread_state.position.store(target_secs, Ordering::SeqCst);
                                if was_playing {
                                    thread_state.start_playback_timer(target_secs);
                                }
                                thread_state.set_stream_error(false);
                                log::info!(
                                    "Audio thread: direct DSD seeked to {}s (was_playing: {})",
                                    target_secs,
                                    was_playing
                                );
                                return;
                            }

                            // Three cases reach this handler:
                            //   * full-file playback (current_audio_data set)
                            //   * CMAF streaming, download complete (buffered
                            //     source holds the full file)
                            //   * CMAF streaming, download IN PROGRESS — only
                            //     allowed if the target position falls inside
                            //     the already-buffered region. skip_duration
                            //     reads samples sequentially, so seeking past
                            //     the watermark would block the audio thread
                            //     waiting for the rest of the download.
                            //     Cache, offline-cache, and local-library
                            //     playback reach this handler with
                            //     current_audio_data Some and skip the
                            //     streaming branch entirely (issue #335).
                            if current_audio_data.is_none() && current_streaming_source.is_none() {
                                log::warn!("Audio thread: cannot seek - no audio data available");
                                return;
                            }
                            if let Some(ref stream_src) = *current_streaming_source {
                                if !stream_src.is_complete() {
                                    // Approximate bytes-to-seconds mapping via
                                    // download fraction × total duration. Exact
                                    // for CBR, close-enough for FLAC/VBR; the
                                    // 0.90 margin covers the error band so the
                                    // decoder never reads past the watermark.
                                    let duration_secs = thread_state.duration();
                                    let progress = stream_src.progress().unwrap_or(0.0);
                                    if duration_secs == 0 || progress <= 0.0 {
                                        log::warn!(
                                            "Audio thread: seek to {}s ignored — streaming progress unknown",
                                            position_secs
                                        );
                                        return;
                                    }
                                    let max_seekable_secs =
                                        (progress * 0.90 * duration_secs as f32) as u64;
                                    if position_secs > max_seekable_secs {
                                        log::warn!(
                                            "Audio thread: seek to {}s ignored — past buffered watermark ({}s, progress {:.1}%)",
                                            position_secs,
                                            max_seekable_secs,
                                            progress * 100.0
                                        );
                                        return;
                                    }
                                    log::info!(
                                        "Audio thread: seek to {}s within buffered zone (watermark {}s, progress {:.1}%)",
                                        position_secs,
                                        max_seekable_secs,
                                        progress * 100.0
                                    );
                                }
                            }

                            let Some(ref stream) = *stream_opt else {
                                log::error!(
                                    "Audio thread: cannot seek - no audio device available"
                                );
                                return;
                            };

                            log::info!("Audio thread: seeking to {}s", position_secs);

                            // After take(), every failure path must clear playing
                            // state — otherwise UI can show "playing" with no engine.
                            if let Some(engine) = current_engine.take() {
                                engine.stop();
                            }
                            thread_state.set_hardware_volume_active(false);
                            let seek_abort = |thread_state: &SharedState, why: &str| {
                                log::error!("Audio thread: seek aborted: {why}");
                                thread_state.is_playing.store(false, Ordering::SeqCst);
                                thread_state.set_stream_error(true);
                            };

                            let mut engine = match stream {
                                StreamType::Rodio { sink: mixer_sink, .. } => {
                                    match PlaybackEngine::new_rodio(&mixer_sink.mixer()) {
                                        Ok(e) => e,
                                        Err(e) => {
                                            seek_abort(
                                                &thread_state,
                                                &format!("rodio engine create failed: {e}"),
                                            );
                                            return;
                                        }
                                    }
                                }
                                StreamType::Direct(alsa_stream) => {
                                    let (hardware_volume, hardware_volume_events) =
                                        direct_hardware_volume_binding(
                                            &thread_settings,
                                            &thread_state,
                                        );
                                    PlaybackEngine::new_direct(
                                        alsa_stream.clone(),
                                        hardware_volume,
                                        thread_viz_tap.clone(),
                                        hardware_volume_events,
                                    )
                                }
                                #[cfg(target_os = "linux")]
                                StreamType::Jack(jack_stream) => {
                                    PlaybackEngine::new_jack(jack_stream.clone())
                                }
                            };

                            let volume = f32::from_bits(thread_state.volume.load(Ordering::SeqCst));
                            apply_engine_volume(&stream_opt, &engine, volume);

                            // Build the decoded source for the seek. Both
                            // streaming and cached paths use Symphonia's native
                            // seek — FLAC seek table / MP3 TOC jumps straight
                            // to the target byte, then decodes forward to the
                            // exact sample. Avoids skip_duration's
                            // decode-every-sample-from-zero loop, which stalls
                            // the audio thread on long seeks (especially FLAC
                            // Hi-Res). Cached path falls back to decode_with_
                            // fallback + skip_duration if Symphonia can't
                            // probe the format (e.g., rodio-only MP4/AAC),
                            // preserving existing behavior for those cases.
                            let skip_duration = Duration::from_secs(position_secs);
                            let skipped_source: Box<dyn Source<Item = f32> + Send> = if let Some(
                                ref stream_src,
                            ) =
                                *current_streaming_source
                            {
                                match IncrementalStreamingSource::new(stream_src.clone()) {
                                    Ok(mut s) => {
                                        if let Err(e) = s.seek_to(skip_duration) {
                                            seek_abort(
                                                &thread_state,
                                                &format!("streaming native seek failed: {e}"),
                                            );
                                            return;
                                        }
                                        Box::new(s)
                                    }
                                    Err(e) => {
                                        seek_abort(
                                            &thread_state,
                                            &format!("streaming source for seek failed: {e}"),
                                        );
                                        return;
                                    }
                                }
                            } else {
                                let audio_data = current_audio_data
                                    .as_ref()
                                    .expect("current_audio_data was checked Some above");
                                match InMemorySource::from_shared(audio_data.clone()) {
                                    Ok(mut s) => match s.seek_to(skip_duration) {
                                        Ok(()) => Box::new(s),
                                        Err(e) => {
                                            log::warn!(
                                                    "Native seek on cached source failed ({}); falling back to skip_duration",
                                                    e
                                                );
                                            match decode_with_fallback(audio_data) {
                                                Ok(fb) => Box::new(fb.skip_duration(skip_duration)),
                                                Err(e) => {
                                                    seek_abort(
                                                        &thread_state,
                                                        &format!(
                                                            "decode for seek failed: {e}"
                                                        ),
                                                    );
                                                    return;
                                                }
                                            }
                                        }
                                    },
                                    Err(e) => {
                                        log::warn!(
                                                "InMemorySource probe failed ({}); falling back to skip_duration",
                                                e
                                            );
                                        match decode_with_fallback(audio_data) {
                                            Ok(fb) => Box::new(fb.skip_duration(skip_duration)),
                                            Err(e) => {
                                                seek_abort(
                                                    &thread_state,
                                                    &format!(
                                                        "decode for seek failed: {e}"
                                                    ),
                                                );
                                                return;
                                            }
                                        }
                                    }
                                }
                            };

                            // Send Reset to analyzer (seek invalidates accumulated samples)
                            let _ = analyzer_tx.try_send(AnalyzerMessage::Reset);

                            // Wrap source with diagnostic, normalization, and visualizer
                            // Reuse the gain + atomic from the current track
                            let skipped_source = wrap_source(
                                skipped_source,
                                *current_normalization_gain,
                                current_gain_atomic.clone(),
                                &analyzer_tx,
                                &analyzer_enabled,
                                None,
                                position_secs.saturating_mul(
                                    (*current_track_sample_rate).unwrap_or(0) as u64,
                                ),
                            );
                            if let Err(e) = engine.append(skipped_source) {
                                seek_abort(
                                    &thread_state,
                                    &format!("append source for seek failed: {e}"),
                                );
                                return;
                            }

                            let was_playing = thread_state.is_playing.load(Ordering::SeqCst);
                            if !was_playing {
                                engine.pause();
                            }

                            thread_state.position.store(position_secs, Ordering::SeqCst);
                            if was_playing {
                                thread_state.start_playback_timer(position_secs);
                            }

                            *current_engine = Some(engine);
                            thread_state.set_stream_error(false);
                            log::info!(
                                "Audio thread: seeked to {}s (was_playing: {})",
                                position_secs,
                                was_playing
                            );
                        }
                        AudioCommand::ReinitDevice {
                            device_name: new_device,
                        } => {
                            log::info!(
                                "Audio thread: reinitializing device (new: {:?})",
                                new_device
                            );
                            *pause_suspend_deadline = None;

                            if let Some(engine) = current_engine.take() {
                                engine.stop();
                            }
                            thread_state.set_hardware_volume_active(false);

                            drop(stream_opt.take());
                            log::info!("Audio thread: previous stream dropped, device released");

                            // ReloadSettings updates `thread_settings` before
                            // this command is queued. When the NEW route is no
                            // longer ALSA Direct, finish the teardown the same
                            // way ReleaseDevice does: dropping the PCM alone
                            // releases the fd/reservation but leaves the
                            // PipeWire sink QBZ suspended. Keep it suspended
                            // for direct->direct reinitialization so PipeWire
                            // cannot race the immediate exclusive reopen.
                            #[cfg(target_os = "linux")]
                            {
                                let next_is_alsa_direct = thread_settings
                                    .lock()
                                    .ok()
                                    .map(|settings| {
                                        qbz_audio::alsa_direct::uses_alsa_direct_route(&settings)
                                    })
                                    .unwrap_or(false);
                                if !next_is_alsa_direct {
                                    if let Err(error) =
                                        qbz_audio::alsa_backend::resume_suspended_sink()
                                    {
                                        log::warn!(
                                            "Audio thread: release during reinit was incomplete: {error}"
                                        );
                                    }
                                }
                                qbz_audio::pipewire_backend::PipeWireBackend::reset_pipewire_clock();
                            }

                            std::thread::sleep(Duration::from_millis(100));

                            *current_device_name = new_device;
                            // Use last known sample rate/channels to maintain DAC passthrough
                            let sr = current_track_sample_rate.unwrap_or(48000);
                            let ch = current_track_channels.unwrap_or(2);
                            log::info!("ReinitDevice: reinitializing at {}Hz/{}ch", sr, ch);
                            *stream_opt = init_device(current_device_name, &thread_state, sr, ch);

                            if stream_opt.is_some() {
                                log::info!("Audio thread: device reinitialized successfully");
                                *consecutive_sink_failures = 0;
                            } else {
                                log::error!("Audio thread: failed to reinitialize device");
                            }

                            // Preserve position so Resume can seek back to it.
                            // pause_playback_timer() captures the real-time position
                            // into thread_state.position before clearing the timer.
                            thread_state.pause_playback_timer();
                            thread_state.is_playing.store(false, Ordering::SeqCst);
                            // Keep current_audio_data and current_streaming_source
                            // intact so Resume can recreate the engine and seek.
                        }
                        AudioCommand::ReleaseDevice { completed } => {
                            log::info!("Audio thread: releasing output device (user-requested)");
                            // Cancel any deferred drop and tear the stream down NOW so
                            // the device is freed immediately (no warm-stream lingering).
                            *pause_suspend_deadline = None;
                            if let Some(engine) = current_engine.take() {
                                engine.stop();
                            }
                            thread_state.set_hardware_volume_active(false);
                            drop(stream_opt.take());
                            // Undo anything QBZ parked so PipeWire / WirePlumber can
                            // reclaim the device (e.g. a DAC left invisible to other
                            // apps after bit-perfect ALSA Direct held it exclusively).
                            // Both calls are self-gating no-ops if QBZ didn't set them.
                            #[cfg(target_os = "linux")]
                            let release_result = {
                                let result = qbz_audio::alsa_backend::resume_suspended_sink();
                                qbz_audio::pipewire_backend::PipeWireBackend::reset_pipewire_clock();
                                result
                            };
                            #[cfg(not(target_os = "linux"))]
                            let release_result = Ok(());
                            thread_state.pause_playback_timer();
                            thread_state.is_playing.store(false, Ordering::SeqCst);
                            // Keep current_audio_data / current_streaming_source intact
                            // so a later Play / Resume reopens and continues.
                            log::info!("Audio thread: output device released");
                            let _ = completed.send(release_result);
                        }
                        AudioCommand::PlayNext {
                            data,
                            track_id,
                            sample_rate,
                            channels,
                            bit_depth,
                        } => {
                            // Gapless: append next track to existing Rodio Sink
                            let engine = match current_engine.as_mut() {
                                Some(e) => e,
                                None => {
                                    log::warn!(
                                        "Gapless: no engine, ignoring PlayNext for track {}",
                                        track_id
                                    );
                                    thread_state.set_gapless_ready(false);
                                    return;
                                }
                            };

                            // Verify format compatibility (same sample rate and channels)
                            if let (Some(cur_sr), Some(cur_ch)) =
                                (*current_track_sample_rate, *current_track_channels)
                            {
                                if sample_rate != cur_sr || channels != cur_ch {
                                    log::info!(
                                    "Gapless: format mismatch (current {}Hz/{}ch vs next {}Hz/{}ch), ignoring PlayNext for track {}",
                                    cur_sr, cur_ch, sample_rate, channels, track_id
                                );
                                    thread_state.set_gapless_ready(false);
                                    return;
                                }
                            }

                            // Decode the next track's audio
                            let source = match decode_with_fallback(&data) {
                                Ok(s) => s,
                                Err(e) => {
                                    log::error!(
                                        "Gapless: failed to decode track {}: {}",
                                        track_id,
                                        e
                                    );
                                    thread_state.set_gapless_ready(false);
                                    return;
                                }
                            };

                            let actual_duration =
                                source.total_duration().map(|d| d.as_secs()).unwrap_or(0);

                            // Start gain for the next track, resolved before
                            // its first sample (normalization.rs).
                            let plan = normalization::plan_start_gain(
                                &thread_settings,
                                &loudness_cache,
                                track_id,
                                Some(&data),
                            );
                            let (normalization, gain_atomic) =
                                (plan.normalization, plan.gain_atomic.clone());

                            // Wrap source with normalization/visualizer pipeline
                            let pending_gain_atomic = gain_atomic.clone();
                            let source = wrap_source(
                                source,
                                normalization,
                                gain_atomic.clone(),
                                &analyzer_tx,
                                &analyzer_enabled,
                                Some(AnalyzerWaveformTrack {
                                    track_id,
                                    sample_rate,
                                    channels,
                                    duration_secs: actual_duration,
                                    start_frame: 0,
                                    target_lufs: plan.target_lufs,
                                    gain_atomic,
                                    known_gain: plan.known_gain,
                                    prevent_clipping: plan.prevent_clipping,
                                }),
                                0,
                            );

                            // Append to existing Sink (gapless queue).
                            // Late-gapless race: this append can arrive AFTER
                            // the engine went empty at natural end ("track
                            // finished" already set thread_state.is_playing =
                            // false). The engine's own append re-opens its
                            // writer gate and the track PLAYS, but the
                            // player-level flag stays false — freezing the
                            // position monitor, the next gapless arm, the
                            // QConnect reports and the queue cursor on the
                            // finished track while the next one plays (the
                            // "invisible track" desync). Capture emptiness
                            // BEFORE the append: a user-paused engine still
                            // holds its current source (NOT empty), so a
                            // paused session is never resumed by this.
                            let engine_was_empty = engine.empty();
                            if let Err(e) = engine.append(source) {
                                log::error!(
                                    "Gapless: failed to append track {} to engine: {}",
                                    track_id,
                                    e
                                );
                                thread_state.set_gapless_ready(false);
                                return;
                            }
                            if engine_was_empty
                                && !thread_state.is_playing.load(Ordering::SeqCst)
                            {
                                log::info!(
                                    "Gapless: PlayNext landed after track finished — resuming playback-state tracking"
                                );
                                thread_state.is_playing.store(true, Ordering::SeqCst);
                            }

                            // Store pending gapless data for transition detection
                            log::info!("[gapless-trace] set (pcm) track {track_id}");
                            *gapless_pending = Some(GaplessPending {
                                track_id,
                                play_generation: thread_state.current_play_generation(),
                                duration_secs: actual_duration,
                                media: GaplessMedia::Buffered(Arc::new(data)),
                                sample_rate,
                                channels,
                                bit_depth,
                                normalization_gain: normalization,
                                gain_atomic: pending_gain_atomic,
                            });
                            thread_state.set_gapless_next_track_id(track_id);
                            thread_state.set_gapless_ready(false); // Request fulfilled

                            log::info!(
                                "Gapless: queued track {} (duration: {}s) for seamless transition",
                                track_id,
                                actual_duration
                            );
                        }
                        AudioCommand::PlayNextStreaming {
                            source,
                            track_id,
                            sample_rate,
                            channels,
                            bit_depth,
                            duration_secs,
                        } => {
                            let engine = match current_engine.as_mut() {
                                Some(engine) => engine,
                                None => {
                                    log::warn!(
                                        "Gapless stream: no engine for successor {}",
                                        track_id
                                    );
                                    thread_state.cancel_gapless_stream_feeder(track_id);
                                    thread_state.set_gapless_ready(false);
                                    return;
                                }
                            };
                            if let (Some(cur_sr), Some(cur_ch)) =
                                (*current_track_sample_rate, *current_track_channels)
                            {
                                if sample_rate != cur_sr || channels != cur_ch {
                                    log::info!(
                                        "Gapless stream: format mismatch (current {}Hz/{}ch vs next {}Hz/{}ch), ignoring track {}",
                                        cur_sr,
                                        cur_ch,
                                        sample_rate,
                                        channels,
                                        track_id
                                    );
                                    thread_state.cancel_gapless_stream_feeder(track_id);
                                    thread_state.set_gapless_ready(false);
                                    return;
                                }
                            }
                            if !source.has_min_buffer() {
                                log::warn!(
                                    "Gapless stream: initial buffer not ready for track {}",
                                    track_id
                                );
                                thread_state.cancel_gapless_stream_feeder(track_id);
                                thread_state.set_gapless_ready(false);
                                return;
                            }

                            let incremental = match IncrementalStreamingSource::new_for_play(
                                source.clone(),
                                PlaybackBufferReporter::new(
                                    thread_state.clone(),
                                    track_id,
                                    thread_state.current_play_generation(),
                                ),
                            ) {
                                Ok(source) => source,
                                Err(error) => {
                                    log::warn!(
                                        "Gapless stream: decoder setup failed for track {}: {}",
                                        track_id,
                                        error
                                    );
                                    thread_state.cancel_gapless_stream_feeder(track_id);
                                    thread_state.set_gapless_ready(false);
                                    return;
                                }
                            };
                            let buffered = source.get_buffered_data();
                            let plan = normalization::plan_start_gain(
                                &thread_settings,
                                &loudness_cache,
                                track_id,
                                buffered.as_deref(),
                            );
                            let (normalization, gain_atomic) =
                                (plan.normalization, plan.gain_atomic.clone());
                            let pending_gain_atomic = gain_atomic.clone();
                            let source_to_play = wrap_source(
                                Box::new(incremental),
                                normalization,
                                gain_atomic.clone(),
                                &analyzer_tx,
                                &analyzer_enabled,
                                Some(AnalyzerWaveformTrack {
                                    track_id,
                                    sample_rate,
                                    channels,
                                    duration_secs,
                                    start_frame: 0,
                                    target_lufs: plan.target_lufs,
                                    gain_atomic,
                                    known_gain: plan.known_gain,
                                    prevent_clipping: plan.prevent_clipping,
                                }),
                                0,
                            );
                            let engine_was_empty = engine.empty();
                            if let Err(error) = engine.append(source_to_play) {
                                log::warn!(
                                    "Gapless stream: append failed for track {}: {}",
                                    track_id,
                                    error
                                );
                                thread_state.cancel_gapless_stream_feeder(track_id);
                                thread_state.set_gapless_ready(false);
                                return;
                            }
                            if engine_was_empty
                                && !thread_state.is_playing.load(Ordering::SeqCst)
                            {
                                thread_state.is_playing.store(true, Ordering::SeqCst);
                            }

                            log::info!("[gapless-trace] set (stream) track {track_id}");
                            *gapless_pending = Some(GaplessPending {
                                track_id,
                                play_generation: thread_state.current_play_generation(),
                                duration_secs,
                                media: GaplessMedia::Streaming(source),
                                sample_rate,
                                channels,
                                bit_depth,
                                normalization_gain: normalization,
                                gain_atomic: pending_gain_atomic,
                            });
                            thread_state.set_gapless_next_track_id(track_id);
                            thread_state.set_gapless_ready(false);
                            log::info!(
                                "Gapless stream: queued track {} after its initial buffer",
                                track_id
                            );
                        }
                    }
                };

            loop {
                // #660: observe a wedged output stream at the TOP of every loop
                // iteration, before the command wait. Under QConnect the
                // renderer-state command flow keeps recv_timeout returning
                // Ok(...), so the Timeout arm (where this used to live) never
                // fired — on the Pi a wedged cpal worker spun a core for 20+ s
                // until an unrelated command arrived. Here it runs regardless of
                // command flow and of is_playing. The stream is always dropped
                // (that joins cpal's spinning worker); we only rebuild when we
                // were actually playing.
                if let Some(reason) = qbz_audio::stream_health::take_wedged() {
                    let now = Instant::now();
                    let was_playing = thread_state.is_playing.load(Ordering::SeqCst);
                    let resume_at = thread_state.current_position();
                    if let Some(engine) = current_engine.take() {
                        engine.stop();
                    }
                    drop(stream_opt.take());
                    qbz_audio::stream_health::clear_wedged();
                    gapless_pending = None;
                    gapless_request_armed = false;
                    if !was_playing {
                        log::warn!(
                            "Audio thread: output stream wedged while stopped ({reason}); released it"
                        );
                    } else {
                        match wedge_recovery(last_wedge_rebuild, now) {
                            WedgeRecovery::Rebuild => {
                                last_wedge_rebuild = Some(now);
                                log::warn!(
                                    "Audio thread: output stream wedged ({reason}); rebuilding it and resuming at {resume_at}s"
                                );
                                let sr = current_track_sample_rate.unwrap_or(48000);
                                let ch = current_track_channels.unwrap_or(2);
                                stream_opt =
                                    init_device(&current_device_name, &thread_state, sr, ch);
                                if stream_opt.is_some() {
                                    consecutive_sink_failures = 0;
                                    handle_command(
                                        AudioCommand::Seek(resume_at),
                                        &mut current_engine,
                                        &mut current_audio_data,
                                        &mut current_streaming_source,
                                        &mut current_direct_dsd,
                                        &mut stream_opt,
                                        &mut current_device_name,
                                        &mut consecutive_sink_failures,
                                        &mut pause_suspend_deadline,
                                        &mut current_track_sample_rate,
                                        &mut current_track_channels,
                                        &mut current_normalization_gain,
                                        &mut current_gain_atomic,
                                        &mut gapless_pending,
                                        &mut gapless_request_armed,
                                    );
                                }
                                if current_engine.is_none() {
                                    log::error!(
                                        "Audio thread: could not rebuild the output stream after it wedged ({reason})"
                                    );
                                    thread_state
                                        .record_stream_error(format!("Audio output stopped: {reason}"));
                                    thread_state.pause_playback_timer();
                                    thread_state.is_playing.store(false, Ordering::SeqCst);
                                }
                            }
                            WedgeRecovery::Stop => {
                                last_wedge_rebuild = Some(now);
                                log::error!(
                                    "Audio thread: output stream wedged again within {}s ({reason}); releasing the device and stopping",
                                    WEDGE_REBUILD_BACKOFF.as_secs()
                                );
                                thread_state
                                    .record_stream_error(format!("Audio output stopped: {reason}"));
                                thread_state.pause_playback_timer();
                                thread_state.is_playing.store(false, Ordering::SeqCst);
                            }
                        }
                    }
                }
                thread_state.publish_output_stream_format(stream_opt.as_ref());
                if thread_state.is_playing.load(Ordering::SeqCst) {
                    match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(command) => handle_command(
                            command,
                            &mut current_engine,
                            &mut current_audio_data,
                            &mut current_streaming_source,
                            &mut current_direct_dsd,
                            &mut stream_opt,
                            &mut current_device_name,
                            &mut consecutive_sink_failures,
                            &mut pause_suspend_deadline,
                            &mut current_track_sample_rate,
                            &mut current_track_channels,
                            &mut current_normalization_gain,
                            &mut current_gain_atomic,
                            &mut gapless_pending,
                            &mut gapless_request_armed,
                        ),
                        Err(RecvTimeoutError::Timeout) => {
                            let now = Instant::now();
                            if now.duration_since(last_empty_check) >= Duration::from_millis(500) {
                                last_empty_check = now;

                                // Update streaming buffer progress for UI seekbar
                                if let Some(streaming_src) = current_streaming_source.as_ref() {
                                    let progress = streaming_src.progress().unwrap_or(1.0);
                                    thread_state.set_buffer_progress(progress);
                                } else {
                                    thread_state.set_buffer_progress(0.0);
                                }

                                // Streaming -> cached promotion:
                                // once streaming download completes, persist full data and clear streaming marker.
                                // This unlocks normal gapless pre-queue for the current track's tail.
                                let mut clear_streaming_source = false;
                                if let Some(streaming_src) = current_streaming_source.as_ref() {
                                    if streaming_src.is_complete() {
                                        if current_audio_data.is_none() {
                                            // The sealed source shares its allocation with the
                                            // active decoder. Promotion is constant-time even
                                            // for a track larger than the entire L1 budget.
                                            if let Some(full_data) = streaming_src.complete_data_shared() {
                                                log::info!(
                                                    "Streaming promotion: sharing full track ({} bytes), enabling cached transition path",
                                                    full_data.len()
                                                );
                                                current_audio_data = Some(full_data);
                                                clear_streaming_source = true;
                                            }
                                        } else {
                                            clear_streaming_source = true;
                                        }
                                    }
                                }
                                if clear_streaming_source {
                                    current_streaming_source = None;
                                }

                                let pos = thread_state.current_position();
                                let dur = thread_state.duration.load(Ordering::SeqCst);

                                // Track whether a gapless transition fired in this iteration
                                // so the "approaching end" check below can skip itself.
                                // Without this, the stale pos/dur snapshot above would arm
                                // gapless_request_armed=true for the new track immediately
                                // (because pos/dur still point at the outgoing track at the
                                // moment of swap), and the flag never resets during the new
                                // track's playback — so the real "approaching end" trigger
                                // for the new track never fires and gapless playback stalls
                                // out at engine-empty.
                                let mut transition_consumed_pending = false;

                                // Gapless transition detection: when position exceeds current
                                // track duration, the queued next track has started playing.
                                //
                                // ORDERING NOTE: the polling loop in lib.rs reads `track_id`,
                                // `gapless_next_track_id`, and other fields as separate atomic
                                // loads, so it can observe an inconsistent intermediate state
                                // if these stores aren't ordered carefully. The frontend's
                                // `isGaplessTransition` predicate requires
                                // `event.gapless_next_track_id === 0` AND
                                // `event.track_id !== currentTrack.id`. If the polling loop
                                // reads `track_id` post-swap but `gapless_next_track_id`
                                // pre-reset, both conditions can't be satisfied simultaneously
                                // and the frontend mis-classifies the gapless transition as an
                                // external track change — leaving the UI stuck on the previous
                                // title while the audio plays the new track.
                                //
                                // To eliminate that race, clear the "transition complete"
                                // markers (`gapless_next_track_id`, `gapless_ready`) BEFORE
                                // mutating `track_id`. Any racing reader either sees the old
                                // track_id with cleared slots (no transition observed yet, will
                                // catch up on next tick) or the new track_id with cleared slots
                                // (clean gapless transition), but never the inconsistent
                                // mid-swap mix.
                                if let Some(ref pending) = gapless_pending {
                                    // A streaming reader can only hand off
                                    // after its feeder has sealed the source.
                                    // The wall-clock timer may reach catalog
                                    // duration while a stalled feeder is still
                                    // alive; promoting then would drop its last
                                    // cancel sender and truncate the outgoing
                                    // tail. Cached sources have no such gate.
                                    let outgoing_stream_sealed = current_streaming_source
                                        .as_ref()
                                        .map(|source| {
                                            source.is_complete()
                                                || source.download_error().is_some()
                                        })
                                        .unwrap_or(true);
                                    if dur > 0 && pos >= dur && outgoing_stream_sealed {
                                        log::info!(
                                            "Gapless transition: track {} -> {} (pos {}s >= dur {}s)",
                                            thread_state.current_track_id.load(Ordering::SeqCst),
                                            pending.track_id, pos, dur
                                        );
                                        // Clear gapless slot markers FIRST so a racing reader
                                        // never sees the inconsistent track_id-changed +
                                        // slot-still-set combination.
                                        thread_state.set_gapless_next_track_id(0);
                                        thread_state.set_gapless_ready(false);
                                        // Now safe to swap the track identity.
                                        thread_state
                                            .current_track_id
                                            .store(pending.track_id, Ordering::SeqCst);
                                        thread_state
                                            .duration
                                            .store(pending.duration_secs, Ordering::SeqCst);
                                        thread_state.start_playback_timer(0);
                                        match &pending.media {
                                            GaplessMedia::Buffered(data) => {
                                                current_audio_data = Some(data.clone());
                                                current_streaming_source = None;
                                            }
                                            GaplessMedia::Streaming(source) => {
                                                current_audio_data = None;
                                                current_streaming_source = Some(source.clone());
                                                thread_state.promote_gapless_stream_feeder(
                                                    pending.track_id,
                                                );
                                            }
                                            GaplessMedia::Direct(media) => {
                                                current_audio_data = None;
                                                current_streaming_source = None;
                                                current_direct_dsd = Some(media.clone());
                                            }
                                        }
                                        thread_state.adopt_buffered_track(
                                            pending.track_id,
                                            PlaybackBufferState::Ready,
                                            pending.play_generation,
                                        );
                                        current_track_sample_rate = Some(pending.sample_rate);
                                        current_track_channels = Some(pending.channels);
                                        thread_state
                                            .set_stream_quality(pending.sample_rate, pending.bit_depth);
                                        thread_state.set_buffer_progress(0.0);
                                        current_normalization_gain = pending.normalization_gain;
                                        current_gain_atomic = pending.gain_atomic.clone();
                                        thread_state
                                            .set_normalization_gain(pending.normalization_gain);
                                        // GAPLESS LIFECYCLE TRACE (2026-08-10): the `X -> X` self-transition
                                        // survived the PlayStreaming fix, and guessing at the remaining
                                        // path has failed twice. Every set/clear now says who did it.
                                        log::info!("[gapless-trace] clear (transition-consumed) pending was {:?}", gapless_pending.as_ref().map(|p| p.track_id));
                                        gapless_pending = None;
                                        gapless_request_armed = false;
                                        transition_consumed_pending = true;
                                    }
                                }

                                // ALSA Direct gapless: the writer thread signals transitions
                                // via an atomic flag instead of position-based detection.
                                // Same ordering rationale as above.
                                if let Some(ref engine) = current_engine {
                                    if engine.take_source_transition() {
                                        if let Some(ref pending) = gapless_pending {
                                            log::info!(
                                                "ALSA Direct gapless transition: track {} -> {}",
                                                thread_state
                                                    .current_track_id
                                                    .load(Ordering::SeqCst),
                                                pending.track_id
                                            );
                                            thread_state.set_gapless_next_track_id(0);
                                            thread_state.set_gapless_ready(false);
                                            thread_state
                                                .current_track_id
                                                .store(pending.track_id, Ordering::SeqCst);
                                            thread_state
                                                .duration
                                                .store(pending.duration_secs, Ordering::SeqCst);
                                            thread_state.start_playback_timer(0);
                                            match &pending.media {
                                                GaplessMedia::Buffered(data) => {
                                                    current_audio_data = Some(data.clone());
                                                    current_streaming_source = None;
                                                }
                                                GaplessMedia::Streaming(source) => {
                                                    current_audio_data = None;
                                                    current_streaming_source = Some(source.clone());
                                                    thread_state.promote_gapless_stream_feeder(
                                                        pending.track_id,
                                                    );
                                                }
                                                GaplessMedia::Direct(media) => {
                                                    current_audio_data = None;
                                                    current_streaming_source = None;
                                                    current_direct_dsd = Some(media.clone());
                                                }
                                            }
                                            thread_state.adopt_buffered_track(
                                                pending.track_id,
                                                PlaybackBufferState::Ready,
                                                pending.play_generation,
                                            );
                                            current_track_sample_rate = Some(pending.sample_rate);
                                            current_track_channels = Some(pending.channels);
                                            thread_state.set_stream_quality(
                                                pending.sample_rate,
                                                pending.bit_depth,
                                            );
                                            thread_state.set_buffer_progress(0.0);
                                            current_normalization_gain = pending.normalization_gain;
                                            current_gain_atomic = pending.gain_atomic.clone();
                                            thread_state
                                                .set_normalization_gain(pending.normalization_gain);
                                            // GAPLESS LIFECYCLE TRACE (2026-08-10): the `X -> X` self-transition
                                            // survived the PlayStreaming fix, and guessing at the remaining
                                            // path has failed twice. Every set/clear now says who did it.
                                            log::info!("[gapless-trace] clear (alsa-transition-consumed) pending was {:?}", gapless_pending.as_ref().map(|p| p.track_id));
                                            gapless_pending = None;
                                            gapless_request_armed = false;
                                            transition_consumed_pending = true;
                                        }
                                    }
                                }

                                // Gapless readiness: signal frontend that it's
                                // time to prepare the next track.
                                //
                                // Lead time used to be 5s but that's too tight
                                // for offline-cache v2 bundles: the AES-CTR
                                // decrypt of a HiRes track on CPUs WITHOUT
                                // AES-NI runs at ~10 MB/s — a 58 MB track
                                // needs ~6s just to decrypt, which blows past
                                // a 5s window and misses the gapless handoff.
                                //
                                // 10s covers most HiRes tracks even on the
                                // software-AES fallback path, and is
                                // harmless when decrypt is fast (the bytes
                                // just land in L1 a few seconds earlier and
                                // sit there until the engine picks them up).
                                //
                                // If the frontend ever exposes a user setting
                                // for this, just plumb it through
                                // AudioSettings and read here.
                                const GAPLESS_LEAD_SECS: u64 = 10;
                                let gapless_enabled = thread_settings
                                    .lock()
                                    .ok()
                                    .map(|s| s.gapless_enabled)
                                    .unwrap_or(false);
                                if gapless_enabled
                                    && !transition_consumed_pending
                                    && dur > 0
                                    && pos + GAPLESS_LEAD_SECS >= dur
                                    && gapless_pending.is_none()
                                    && !gapless_request_armed
                                    && !thread_state.is_gapless_ready()
                                    && thread_state.get_gapless_next_track_id() == 0
                                {
                                    log::info!("Gapless: approaching end of track ({}s/{}s), requesting next", pos, dur);
                                    thread_state.set_gapless_ready(true);
                                    gapless_request_armed = true;
                                }

                                // Original: check if ALL sources are done (engine empty)
                                if let Some(ref engine) = current_engine {
                                    if engine.empty()
                                        && thread_state.is_playing.load(Ordering::SeqCst)
                                    {
                                        log::info!("Audio thread: track finished (engine empty)");
                                        let ended_track_id = thread_state
                                            .current_track_id
                                            .load(Ordering::SeqCst);
                                        thread_state.is_playing.store(false, Ordering::SeqCst);
                                        let duration = thread_state.duration.load(Ordering::SeqCst);
                                        thread_state.position.store(duration, Ordering::SeqCst);
                                        thread_state
                                            .playback_start_millis
                                            .store(0, Ordering::SeqCst);
                                        // Clear gapless state on track end
                                        thread_state.set_gapless_ready(false);
                                        thread_state.set_gapless_next_track_id(0);
                                        // GAPLESS LIFECYCLE TRACE (2026-08-10): the `X -> X` self-transition
                                        // survived the PlayStreaming fix, and guessing at the remaining
                                        // path has failed twice. Every set/clear now says who did it.
                                        log::info!("[gapless-trace] clear (engine-empty) pending was {:?}", gapless_pending.as_ref().map(|p| p.track_id));
                                        gapless_pending = None;
                                        gapless_request_armed = false;
                                        // Publish only after every stopped/end
                                        // field above is coherent. The
                                        // generation is the release edge the
                                        // pollers key on.
                                        thread_state.record_engine_empty(ended_track_id);
                                        thread_state.set_buffer_state_for_play(
                                            PlaybackBufferState::Idle,
                                            ended_track_id,
                                            thread_state.current_play_generation(),
                                        );
                                    }
                                }
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => {
                            log::info!("Audio thread: channel closed, exiting");
                            break;
                        }
                    }
                } else {
                    if let Some(deadline) = pause_suspend_deadline {
                        // DoP streams are NEVER suspended on pause: the writer
                        // keeps the DAC locked in DSD mode with 0x69 silence,
                        // and there is no current_audio_data to resume from.
                        let dop_active = current_engine
                            .as_ref()
                            .map(|e| e.is_dop())
                            .unwrap_or(false);
                        if stream_opt.is_some() && !dop_active {
                            let now = Instant::now();
                            if now >= deadline {
                                if let Some(engine) = current_engine.take() {
                                    engine.stop();
                                }
                                thread_state.set_hardware_volume_active(false);
                                drop(stream_opt.take());
                                pause_suspend_deadline = None;
                                // Reset the PipeWire clock if WE forced it (self-gating;
                                // see reset_pipewire_clock). Unconditional now — the
                                // previous pw_force_bitperfect gate leaked a forced rate
                                // for plain (no-passthrough) PipeWire users (#263).
                                #[cfg(target_os = "linux")]
                                qbz_audio::pipewire_backend::PipeWireBackend::reset_pipewire_clock();
                                // The exclusive device is now released (stream dropped
                                // above), so resume any PipeWire sink we suspended for
                                // exclusive access — self-gating no-op otherwise (#263).
                                #[cfg(target_os = "linux")]
                                let _ = qbz_audio::alsa_backend::resume_suspended_sink();
                                log::info!("Audio thread: suspended stream after pause");
                                continue;
                            }

                            let wait = deadline.saturating_duration_since(now);
                            let wait = std::cmp::min(wait, Duration::from_millis(250));
                            match rx.recv_timeout(wait) {
                                Ok(command) => handle_command(
                                    command,
                                    &mut current_engine,
                                    &mut current_audio_data,
                                    &mut current_streaming_source,
                                    &mut current_direct_dsd,
                                    &mut stream_opt,
                                    &mut current_device_name,
                                    &mut consecutive_sink_failures,
                                    &mut pause_suspend_deadline,
                                    &mut current_track_sample_rate,
                                    &mut current_track_channels,
                                    &mut current_normalization_gain,
                                    &mut current_gain_atomic,
                                    &mut gapless_pending,
                                    &mut gapless_request_armed,
                                ),
                                Err(RecvTimeoutError::Timeout) => {}
                                Err(RecvTimeoutError::Disconnected) => {
                                    log::info!("Audio thread: channel closed, exiting");
                                    break;
                                }
                            }
                            continue;
                        }
                        pause_suspend_deadline = None;
                    }

                    // #660: a bounded wait (not a blocking recv) so the loop
                    // keeps iterating and the top-of-loop wedge check runs even
                    // while stopped — a stream can wedge with is_playing already
                    // false and cpal's worker would otherwise spin unseen.
                    match rx.recv_timeout(Duration::from_millis(250)) {
                        Ok(command) => handle_command(
                            command,
                            &mut current_engine,
                            &mut current_audio_data,
                            &mut current_streaming_source,
                            &mut current_direct_dsd,
                            &mut stream_opt,
                            &mut current_device_name,
                            &mut consecutive_sink_failures,
                            &mut pause_suspend_deadline,
                            &mut current_track_sample_rate,
                            &mut current_track_channels,
                            &mut current_normalization_gain,
                            &mut current_gain_atomic,
                            &mut gapless_pending,
                            &mut gapless_request_armed,
                        ),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => {
                            log::info!("Audio thread: channel closed, exiting");
                            break;
                        }
                    }
                }
            }
            thread_state.publish_output_stream_format(None);
        });

        // Two-level playback cache: L1 in memory, L2 on disk (~800 MB). The
        // L1 budget comes from the host memory profile pushed down via
        // `memory_tuning` at process start (400 MB Normal / 50 MB LowMemory)
        // — on a 1 GB host the historical 400 MB hardcode alone could eat
        // 40 % of RAM (issue #660). A disk-cache failure degrades to
        // L1-only rather than aborting player creation.
        let l1_max_bytes = memory_tuning::audio_cache_l1_max_bytes();
        let storage_directory = settings.lock().ok()
            .and_then(|s| s.playback_cache.disk_directory.clone());
        let disk_cache = match storage_directory {
            Some(parent) => std::fs::read_dir(&parent)
                .map_err(|e| format!("Selected playback storage is unavailable: {e}"))
                .and_then(|_| qbz_cache::PlaybackCache::with_path(
                    std::path::PathBuf::from(parent).join("qbz-playback"), 800 * 1024 * 1024,
                )),
            None => qbz_cache::PlaybackCache::new(800 * 1024 * 1024),
        };
        let audio_cache = match disk_cache {
            Ok(pc) => Arc::new(qbz_cache::AudioCache::with_playback_cache(
                l1_max_bytes,
                Arc::new(pc),
            )),
            Err(e) => {
                log::warn!("Playback disk cache unavailable: {e}; memory cache only");
                Arc::new(qbz_cache::AudioCache::new(l1_max_bytes))
            }
        };

        if let Ok(config) = settings.lock() {
            set_max_initial_buffer_bytes(config.playback_cache.prefetch_policy(l1_max_bytes).initial_buffer_cap);
            if let Err(error) = audio_cache.configure(config.playback_cache.clone()) {
                log::warn!("Invalid playback cache policy, using defaults: {error}");
            }
        }
        qbz_cache::AudioCache::start_maintenance(&audio_cache);

        Self {
            tx,
            state,
            audio_settings: settings,
            visualizer_tap,
            diagnostic,
            audio_cache,
            loudness_cache,
        }
    }

    /// Start a new play intent; returns the generation token for this intent.
    /// Any earlier in-flight `play_track` whose generation no longer matches
    /// must not start audio. The monotonic counter lives in `SharedState`
    /// (bumped on every new play intent) so a slow `play_track`
    /// (network/CMAF) that finishes after a newer play cannot restart the
    /// previous track (last-writer race on `AudioCommand::Play`), and so the
    /// audio thread can abandon superseded buffer waits (#591).
    fn begin_play(&self) -> u64 {
        let gen = self.state.begin_play();
        // The bump comes first so a concurrent `register_stream_feeder` sees
        // the new generation; then cancel the previous track's feeder —
        // every caller of `begin_play` is a superseding intent (new play or
        // stop), so the old download is by definition abandoned work and
        // only competes with the new track's fetches for the link.
        self.state.cancel_stream_feeder();
        gen
    }

    /// A passing check is a snapshot, not a lock: a newer intent can still
    /// begin between this check and the subsequent AudioCommand send. That
    /// residual window is accepted; closing it would require the generation
    /// to travel inside the audio command itself.
    fn is_current_play(&self, gen: u64) -> bool {
        self.state.is_current_play(gen)
    }

    /// Play a track by ID.
    ///
    /// First attempts the CMAF streaming pipeline (Akamai CDN, encrypted
    /// segments): only the init segment is fetched synchronously to derive
    /// stream parameters, playback starts immediately, and audio segments are
    /// fetched + decrypted + pushed to the streaming buffer in a background
    /// task. If the CMAF setup fails for any reason, falls back to the legacy
    /// `/track/getFileUrl` path (full FLAC download, then `play_data`).
    pub async fn play_track(
        &self,
        client: &QobuzClient,
        track_id: u64,
        quality: Quality,
        start_position_secs: u64,
    ) -> Result<(), String> {
        // Supersede any earlier in-flight play_track for a different intent.
        let gen = self.begin_play();
        log::info!(
            "Player: Starting playback for track {} with quality {:?} (start {}s, gen {gen})",
            track_id,
            quality,
            start_position_secs
        );

        // Cache hit: replay instantly from L1/L2 unless the cached copy is
        // a lower quality than now requested.
        if let Some(cached) = self.audio_cache.get(track_id) {
            if cached_quality_below_requested(&cached.data, quality) {
                log::info!(
                    "[CACHE] Track {} cached below requested {:?} — re-fetching",
                    track_id,
                    quality
                );
            } else {
                // The row is normally there from the first play; one
                // track/get otherwise, so the level is right from sample 0.
                self.ensure_loudness_row(client, track_id).await;
                if !self.is_current_play(gen) {
                    log::info!(
                        "Player: cache-hit play for track {track_id} superseded (gen {gen})"
                    );
                    return Ok(());
                }
                log::info!(
                    "[CACHE HIT] Track {} ({} bytes) — playing from cache",
                    track_id,
                    cached.size_bytes
                );
                // Use apply_play_data so we do not bump generation again.
                let r = self.apply_play_data(cached.data, track_id);
                // Cached tracks play from in-memory data (no streaming resume
                // offset); honor a session-resume position with a best-effort
                // seek once playback has been handed to the audio thread.
                if r.is_ok() && start_position_secs > 0 && self.is_current_play(gen) {
                    let _ = self.seek(start_position_secs);
                }
                return r;
            }
        }

        if let Some((source, meta, duration)) = self.cached_disk_source(track_id, quality)? {
            self.ensure_loudness_row(client, track_id).await;
            if self.is_current_play(gen) {
                return self.apply_completed_source(source, meta, duration, track_id, start_position_secs);
            }
            return Ok(());
        }

        // `streaming_only` suppresses writing the track into the cache.
        let skip_cache = self
            .audio_settings
            .lock()
            .map(|s| s.streaming_only)
            .unwrap_or(false);

        // Try CMAF streaming pipeline first.
        // Only the init segment is fetched synchronously; audio segments
        // stream in a background task.
        log::info!("[CMAF] Attempting CMAF streaming for track {}", track_id);
        // The catalog's ReplayGain rides along with the CMAF setup so the
        // first sample already carries its gain (ensure_loudness_row).
        let (cmaf_setup, _) = tokio::join!(
            qbz_qobuz::cmaf::setup_streaming(client, track_id, quality),
            self.ensure_loudness_row(client, track_id)
        );
        match cmaf_setup {
            Ok(cmaf_info) => {
                if !self.is_current_play(gen) {
                    log::info!(
                        "Player: CMAF setup for track {track_id} superseded (gen {gen})"
                    );
                    return Ok(());
                }
                // Derive stream parameters from init segment metadata.
                let sample_rate = cmaf_info.sampling_rate.unwrap_or(44100);
                let channels = 2u16; // FLAC from Qobuz is always stereo
                let bit_depth = cmaf_info.bit_depth.unwrap_or(16);
                let total_flac_size = cmaf_info.flac_header.len() as u64
                    + cmaf_info
                        .segment_table
                        .iter()
                        .map(|s| s.byte_len as u64)
                        .sum::<u64>();

                // Track duration from the CMAF segment table. The streaming
                // path's position timer clamps `current_position` to the
                // duration it was given, so a zero here freezes the seek bar
                // at 0:00 and blocks auto-advance — derive the real value
                // from the per-segment sample counts.
                let total_samples: u64 = cmaf_info
                    .segment_table
                    .iter()
                    .map(|s| s.sample_count as u64)
                    .sum();
                let duration_secs = if sample_rate > 0 {
                    total_samples / sample_rate as u64
                } else {
                    0
                };

                // Estimate speed from the init segment fetch (conservative:
                // assume ~10 MB/s if init was too fast to measure reliably).
                let speed_mbps = if cmaf_info.init_fetch_ms > 0 {
                    let init_bytes = cmaf_info.flac_header.len() as f64 + 4096.0; // rough init size
                    (init_bytes / (cmaf_info.init_fetch_ms as f64 / 1000.0))
                        / (1024.0 * 1024.0)
                } else {
                    10.0
                };

                log::info!(
                    "[CMAF] Streaming setup: {}Hz, {}-bit, {:.2} MB total, {:.1} MB/s est, {} segments",
                    sample_rate,
                    bit_depth,
                    total_flac_size as f64 / (1024.0 * 1024.0),
                    speed_mbps,
                    cmaf_info.n_segments
                );

                // Create the streaming buffer and start playback immediately.
                // Non-bumping variant: this intent already holds `gen`.
                let buffer_writer = self.apply_play_streaming_dynamic(
                    track_id,
                    sample_rate,
                    channels,
                    bit_depth,
                    total_flac_size,
                    speed_mbps,
                    duration_secs,
                    start_position_secs, // session-resume offset (0 = from start)
                )?;

                // Spawn the background task that fetches + decrypts + pushes
                // audio segments to the buffer.
                let url_template = cmaf_info.url_template.clone();
                let content_key = cmaf_info.content_key;
                let flac_header = cmaf_info.flac_header;
                let n_segments = cmaf_info.n_segments;
                let cache = self.audio_cache.clone();
                let feeder_state = self.state.clone();
                let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
                let slot_writer = buffer_writer.clone();

                tokio::spawn(async move {
                    match Self::cmaf_stream_segments(
                        &url_template,
                        n_segments,
                        content_key,
                        flac_header,
                        buffer_writer,
                        track_id,
                        cache,
                        skip_cache,
                        total_flac_size,
                        cancel_rx,
                    )
                    .await
                    {
                        // `false` = cancelled on supersede; already logged by
                        // the feeder itself, and it is not a completion.
                        Ok(true) => log::info!(
                            "[CMAF-STREAM COMPLETE] Track {}",
                            track_id
                        ),
                        Ok(false) => {}
                        Err(e) => {
                            feeder_state.record_source_failure(track_id, gen);
                            log::error!(
                                "[CMAF-STREAM ERROR] Track {}: {} — queue recovery armed",
                                track_id,
                                e
                            );
                        }
                    }
                });
                // Register for cancel-on-supersede: the next `begin_play`
                // stops this download instead of letting it hold the link.
                self.state
                    .register_stream_feeder(track_id, cancel_tx, slot_writer, gen);

                return Ok(());
            }
            Err(e) => {
                log::warn!(
                    "[CMAF] Streaming setup failed: {}, falling back to legacy download",
                    e
                );
                // Fall through to the legacy download path.
            }
        }

        if !self.is_current_play(gen) {
            log::info!(
                "Player: legacy path for track {track_id} superseded before URL fetch (gen {gen})"
            );
            return Ok(());
        }

        // Legacy fallback: get the stream URL
        log::info!("Player: Getting stream URL...");
        let stream_url = client
            .get_stream_url_with_fallback(track_id, quality)
            .await
            .map_err(|_| {
                log::error!("Player: Failed to get stream URL (details omitted)");
                "Failed to get stream URL".to_string()
            })?;

        if !self.is_current_play(gen) {
            log::info!(
                "Player: legacy download for track {track_id} superseded after URL (gen {gen})"
            );
            return Ok(());
        }

        log::info!("Player: Got stream URL (format: {})", stream_url.mime_type);

        let (source, writer) = self.download_buffered(&stream_url.url).await?;
        if !self.is_current_play(gen) { return Ok(()); }
        if !skip_cache {
            Self::persist_stream(self.audio_cache.clone(), writer, track_id).await?;
        }
        if !self.is_current_play(gen) { return Ok(()); }
        let (meta, duration) = disk_playback::source_metadata(&source)?;
        self.apply_completed_source(source, meta, duration, track_id, start_position_secs)
    }

    /// Queue a cold Qobuz successor as an incremental source. This is the
    /// Streaming-only counterpart to [`Self::fetch_for_gapless`]: only the
    /// initial buffer must win the final 10-second race, so track length no
    /// longer determines whether the transition is seamless.
    pub async fn queue_next_streaming(
        &self,
        client: &QobuzClient,
        track_id: u64,
        quality: Quality,
    ) -> Result<(), String> {
        let gen = self.state.current_play_generation();
        if let Some((source, meta, duration_secs)) = self.cached_disk_source(track_id, quality)? {
            self.tx.send(AudioCommand::PlayNextStreaming {
                source, track_id, sample_rate: meta.sample_rate, channels: meta.channels,
                bit_depth: meta.bit_depth.unwrap_or(16), duration_secs,
            }).map_err(|error| format!("failed to queue cached disk source: {error}"))?;
            return Ok(());
        }

        if let Some(cached) = self.audio_cache.get(track_id) {
            if !cached_quality_below_requested(&cached.data, quality) {
                return self.play_next(cached.data, track_id);
            }
        }
        let cmaf_info = match qbz_qobuz::cmaf::setup_streaming(client, track_id, quality).await {
            Ok(info) => info,
            Err(error) => {
                log::warn!("[GAPLESS-STREAM] CMAF setup failed: {error}; trying buffered legacy source");
                let url = client.get_stream_url_with_fallback(track_id, quality).await.map_err(|e| e.to_string())?;
                let (source, writer) = self.download_buffered(&url.url).await?;
                if !self.is_current_play(gen) { return Err("gapless legacy fetch superseded".into()); }
                let skip_cache = self.audio_settings.lock().map(|s| s.streaming_only).unwrap_or(false);
                if !skip_cache { Self::persist_stream(self.audio_cache.clone(), writer, track_id).await?; }
                if !self.is_current_play(gen) { return Err("gapless legacy fetch superseded".into()); }
                let (meta, duration_secs) = disk_playback::source_metadata(&source)?;
                return self.tx.send(AudioCommand::PlayNextStreaming {
                    source, track_id, sample_rate: meta.sample_rate, channels: meta.channels,
                    bit_depth: meta.bit_depth.unwrap_or(16), duration_secs,
                }).map_err(|e| e.to_string());
            }
        };
        if !self.is_current_play(gen) {
            return Err("current track changed during gapless stream setup".to_string());
        }

        let sample_rate = cmaf_info.sampling_rate.unwrap_or(44_100);
        let channels = 2u16;
        let bit_depth = cmaf_info.bit_depth.unwrap_or(16);
        let total_flac_size = cmaf_info.flac_header.len() as u64
            + cmaf_info
                .segment_table
                .iter()
                .map(|segment| segment.byte_len as u64)
                .sum::<u64>();
        let total_samples: u64 = cmaf_info
            .segment_table
            .iter()
            .map(|segment| segment.sample_count as u64)
            .sum();
        let duration_secs = total_samples / sample_rate.max(1) as u64;
        let speed_mbps = if cmaf_info.init_fetch_ms > 0 {
            let init_bytes = cmaf_info.flac_header.len() as f64 + 4096.0;
            (init_bytes / (cmaf_info.init_fetch_ms as f64 / 1000.0)) / (1024.0 * 1024.0)
        } else {
            10.0
        };

        let mut config = StreamingConfig::from_speed_mbps(speed_mbps);
        let user_secs = self
            .audio_settings
            .lock()
            .map(|settings| settings.stream_buffer_seconds)
            .unwrap_or(2);
        let bytes_per_second = total_flac_size / duration_secs.max(1);
        let user_floor = (user_secs as u64).saturating_mul(bytes_per_second) as usize;
        config.initial_buffer_bytes = config
            .initial_buffer_bytes
            .max(user_floor)
            .clamp(256 * 1024, 8 * 1024 * 1024);

        let (source, writer) = self.streaming_buffer(config, Some(total_flac_size))?;
        let source = Arc::new(source);
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        if !self.state.register_gapless_stream_feeder(
            track_id,
            cancel_tx,
            writer.clone(),
            gen,
        ) {
            return Err("gapless stream was superseded before download".to_string());
        }

        let url_template = cmaf_info.url_template;
        let content_key = cmaf_info.content_key;
        let flac_header = cmaf_info.flac_header;
        let n_segments = cmaf_info.n_segments;
        let cache = self.audio_cache.clone();
        let skip_cache = self.audio_settings.lock().map(|settings| settings.streaming_only).unwrap_or(false);
        tokio::spawn(async move {
            match Self::cmaf_stream_segments(
                &url_template,
                n_segments,
                content_key,
                flac_header,
                writer,
                track_id,
                cache,
                skip_cache,
                total_flac_size,
                cancel_rx,
            )
            .await
            {
                Ok(true) => log::info!("[GAPLESS-STREAM] Track {track_id} fully buffered"),
                Ok(false) => {}
                Err(error) => {
                    log::warn!("[GAPLESS-STREAM] Track {track_id} feeder failed: {error}")
                }
            }
        });

        let wait_started = Instant::now();
        const INITIAL_BUFFER_TIMEOUT: Duration = Duration::from_secs(8);
        while !source.has_min_buffer()
            && source.download_error().is_none()
            && wait_started.elapsed() < INITIAL_BUFFER_TIMEOUT
            && self.is_current_play(gen)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if !self.is_current_play(gen) {
            self.state.cancel_gapless_stream_feeder(track_id);
            return Err("gapless stream superseded during initial buffering".to_string());
        }
        if let Some(error) = source.download_error() {
            self.state.cancel_gapless_stream_feeder(track_id);
            return Err(error);
        }
        if !source.has_min_buffer() {
            self.state.cancel_gapless_stream_feeder(track_id);
            return Err("gapless stream initial buffer timed out".to_string());
        }

        self.tx
            .send(AudioCommand::PlayNextStreaming {
                source,
                track_id,
                sample_rate,
                channels,
                bit_depth,
                duration_secs,
            })
            .map_err(|error| {
                self.state.cancel_gapless_stream_feeder(track_id);
                format!("failed to queue gapless stream: {error}")
            })?;
        log::info!(
            "[GAPLESS-STREAM] Track {} initial buffer ready in {}ms",
            track_id,
            wait_started.elapsed().as_millis()
        );
        Ok(())
    }

    /// Download a track fully into the L1/L2 cache **without** starting
    /// playback.
    ///
    /// Normal cached gapless playback benefits from upcoming tracks being
    /// cache hits so they can be appended fully in-memory. Streaming only
    /// uses [`Self::queue_next_streaming`] instead, which queues a cold
    /// successor after its initial buffer. This method remains the prefetch
    /// primitive the controller drives for the next 1-2 queue tracks.
    ///
    /// CMAF segments and legacy HTTP bodies stream into the selected storage;
    /// oversized tracks never materialize a whole-file Vec for prefetch.
    /// No-ops when `streaming_only` is set, when the track is already
    /// cached, or when another fetch for the same id is in flight.
    pub async fn prefetch_into_cache(
        &self,
        client: &QobuzClient,
        track_id: u64,
        quality: Quality,
    ) -> Result<(), String> {
        // Honor streaming_only — never warm the cache when the user has
        // opted out of caching.
        let skip_cache = self
            .audio_settings
            .lock()
            .map(|s| s.streaming_only)
            .unwrap_or(false);
        if skip_cache {
            log::debug!("[PREFETCH] Skipped track {track_id} — streaming_only mode active");
            return Ok(());
        }

        // Already cached, or another prefetch for this id is already
        // running — nothing to do.
        if self.audio_cache.contains(track_id) {
            log::debug!("[PREFETCH] Track {track_id} already cached");
            return Ok(());
        }
        if self.audio_cache.is_fetching(track_id) {
            log::debug!("[PREFETCH] Track {track_id} already being fetched");
            return Ok(());
        }
        // Back off a track whose last prefetch failed recently (e.g. the account
        // is being 403'd post-outage) instead of re-hammering it on every queue
        // tick — that no-backoff loop is what escalated into an edge/IP block in
        // issue #637. The client-side 403 breaker is the primary guard; this
        // just keeps us from even spinning up the task/log spam.
        const PREFETCH_FAIL_COOLDOWN: Duration = Duration::from_secs(20);
        if self
            .audio_cache
            .recently_failed(track_id, PREFETCH_FAIL_COOLDOWN)
        {
            log::debug!("[PREFETCH] Track {track_id} recently failed — backing off");
            return Ok(());
        }

        if self.audio_cache.get_playback_cache().is_some_and(|cache| cache.contains(track_id)) {
            return Ok(());
        }
        let _fetching = disk_playback::PrefetchGuard::new(self.audio_cache.clone(), track_id);
        log::info!("[PREFETCH] Prefetching track {track_id} at {quality:?}");

        let cmaf_result = async {
            let info = qbz_qobuz::cmaf::setup_streaming(client, track_id, quality).await?;
            let size = info.flac_header.len() as u64 + info.segment_table.iter()
                .map(|segment| segment.byte_len as u64).sum::<u64>();
            let (_source, writer) = self.streaming_buffer(StreamingConfig::fast_start(), Some(size))?;
            let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            Self::cmaf_stream_segments(&info.url_template, info.n_segments, info.content_key,
                info.flac_header, writer, track_id, self.audio_cache.clone(), false, size, cancel_rx).await?;
            Ok::<(), String>(())
        }.await;
        let result = match cmaf_result {
            Ok(()) => Ok(()),
            Err(error) => {
                log::warn!("[PREFETCH] CMAF failed for {track_id}: {error}; trying legacy");
                async {
                    let url = client.get_stream_url_with_fallback(track_id, quality).await.map_err(|e| e.to_string())?;
                    let (_source, writer) = self.download_buffered(&url.url).await?;
                    Self::persist_stream(self.audio_cache.clone(), writer, track_id).await
                }.await
            }
        };
        match result {
            Ok(()) => {
                self.audio_cache.clear_failed(track_id);
                log::info!("[PREFETCH] Complete for track {track_id}");
                // While the bytes are ours and the audio thread is busy with
                // the current track: the catalog row, else a full analysis.
                self.pre_analyze_loudness(client, track_id).await;
                Ok(())
            }
            Err(error) => {
                self.audio_cache.mark_failed(track_id);
                Err(error)
            }
        }
    }

    /// Make sure the loudness cache knows `track_id` before its first sample:
    /// one `track/get` for the catalog's own ReplayGain, only while
    /// normalization is on and nothing is stored yet — so once per track, ever.
    /// Returns whether a row exists afterwards.
    async fn ensure_loudness_row(&self, client: &QobuzClient, track_id: u64) -> bool {
        let enabled = self
            .audio_settings
            .lock()
            .map(|s| s.normalization_enabled)
            .unwrap_or(false);
        if !enabled {
            return false;
        }
        if self.loudness_cache.lookup(track_id).is_some() {
            return true;
        }
        let track = match client.get_track(track_id).await {
            Ok(track) => track,
            Err(e) => {
                log::debug!("[LOUDNESS] track {track_id}: track/get failed: {e}");
                return false;
            }
        };
        let rg = track.audio_info.as_ref().and_then(|a| {
            a.replaygain_track_gain.map(|g| {
                (
                    g as f32,
                    a.replaygain_track_peak
                        .map(|p| p as f32)
                        .filter(|p| *p > 0.0),
                )
            })
        });
        match rg {
            Some((gain_db, peak)) => {
                let stored = self.loudness_cache.store(
                    track_id,
                    lufs_from_replaygain(gain_db),
                    peak,
                    LoudnessSource::ReplayGain,
                );
                log::info!(
                    "[LOUDNESS] track {track_id}: catalog ReplayGain {gain_db:+.2} dB, peak {peak:?}{}",
                    if stored { "" } else { " (a better row already exists)" }
                );
                true
            }
            None => {
                log::info!("[LOUDNESS] track {track_id}: the catalog carries no ReplayGain");
                false
            }
        }
    }

    /// Off the audio thread, right after a prefetch landed in the cache: the
    /// catalog row first (one request); only when the catalog has none, a
    /// full decode + EBU R128 + true-peak measurement of the cached bytes in
    /// a blocking task, skipped on low-memory hosts. The bytes are read back
    /// from the cache (the prefetch streams them straight in), so the
    /// analysis holds one transient copy and drops it.
    async fn pre_analyze_loudness(&self, client: &QobuzClient, track_id: u64) {
        if self.ensure_loudness_row(client, track_id).await {
            return;
        }
        let enabled = self
            .audio_settings
            .lock()
            .map(|s| s.normalization_enabled)
            .unwrap_or(false);
        if !enabled || is_low_memory_class() {
            return;
        }
        let bytes = match self.audio_cache.get(track_id) {
            Some(cached) => cached.data,
            None => match self
                .audio_cache
                .get_playback_cache()
                .and_then(|cache| cache.get(track_id))
            {
                Some(data) => data,
                None => {
                    log::debug!("[LOUDNESS] track {track_id}: no cached bytes to pre-analyse");
                    return;
                }
            },
        };
        let shared = Arc::new(bytes);
        let started = Instant::now();
        match tokio::task::spawn_blocking(move || loudness_scan::measure_bytes(&shared)).await {
            Ok(Ok(m)) => {
                self.loudness_cache
                    .store(track_id, m.lufs, m.true_peak, LoudnessSource::Ebur128Full);
                log::info!(
                    "[LOUDNESS] track {track_id}: pre-analysed {:.1} LUFS, true peak {:?}, {} frames in {} ms",
                    m.lufs,
                    m.true_peak,
                    m.frames,
                    started.elapsed().as_millis()
                );
            }
            Ok(Err(e)) => log::warn!("[LOUDNESS] track {track_id}: pre-analysis failed: {e}"),
            Err(e) => log::warn!("[LOUDNESS] track {track_id}: pre-analysis task failed: {e}"),
        }
    }

    /// True if `track_id` is present in the L1/L2 playback cache. Used by
    /// the gapless controller to decide whether a track can be queued for
    /// a seamless handoff.
    pub fn is_track_cached(&self, track_id: u64) -> bool {
        self.audio_cache.contains(track_id)
    }

    /// Drop every cached audio byte (L1 memory + L2 disk). Called when the
    /// streaming-quality preference changes so the new tier takes effect on the
    /// next play/cast instead of on the next cache miss — the cache is keyed by
    /// track id alone and carries no quality dimension.
    pub fn clear_audio_cache(&self) {
        self.audio_cache.clear();
    }

    /// Drop only the L1 in-memory entries, keeping the L2 disk cache. The
    /// memory-pressure watchdog's relief valve (issue #660): freeing RAM is
    /// what relieves the pressure; the disk files cost nothing to keep.
    pub fn evict_l1_audio_cache(&self) {
        self.audio_cache.evict_all_memory();
    }

    /// Fetch a track's audio bytes for a gapless handoff: L1 memory →
    /// L2 disk → CMAF `download_full` (legacy full download as fallback).
    /// Does not start playback — the caller passes the bytes to
    /// `play_next`. Returns `None` only when every tier fails.
    ///
    /// Ports the L1/L2/CMAF tiers of Tauri's `v2_play_next_gapless`; the
    /// ephemeral / offline-cache / local-library tiers are intentionally
    /// omitted as they do not exist in the Slint MVP.
    pub async fn fetch_for_gapless(
        &self,
        client: &QobuzClient,
        track_id: u64,
        quality: Quality,
    ) -> Option<Vec<u8>> {
        // L1: in-memory cache.
        if let Some(cached) = self.audio_cache.get(track_id) {
            log::info!(
                "[GAPLESS] Track {track_id} from MEMORY cache ({} bytes)",
                cached.size_bytes
            );
            return Some(cached.data);
        }

        // L2: on-disk plain-FLAC playback cache. Warm L1 on the way out.
        if let Some(playback_cache) = self.audio_cache.get_playback_cache() {
            if let Some(audio_data) = playback_cache.get(track_id) {
                log::info!(
                    "[GAPLESS] Track {track_id} from DISK cache ({} bytes)",
                    audio_data.len()
                );
                self.audio_cache.promote_from_disk(track_id, audio_data.clone());
                return Some(audio_data);
            }
        }

        // CMAF full download (Akamai CDN), legacy full download as
        // fallback. Warm L1 so a re-gapless / replay skips the network.
        // The catalog row rides along with the download (once per track).
        let (downloaded, _) = tokio::join!(
            qbz_qobuz::cmaf::download_full(client, track_id, quality),
            self.ensure_loudness_row(client, track_id)
        );
        let downloaded = match downloaded {
            Ok(data) => Some(data),
            Err(e) => {
                log::warn!("[GAPLESS] CMAF failed for track {track_id}: {e}, trying legacy");
                match client.get_stream_url_with_fallback(track_id, quality).await {
                    Ok(stream_url) => match self.download_audio(&stream_url.url).await {
                        Ok(data) => Some(data),
                        Err(e) => {
                            log::warn!("[GAPLESS] Legacy download failed for {track_id}: {e}");
                            None
                        }
                    },
                    Err(e) => {
                        log::warn!("[GAPLESS] No stream URL for {track_id}: {e}");
                        None
                    }
                }
            }
        };

        if let Some(ref data) = downloaded {
            log::info!(
                "[GAPLESS] Track {track_id} downloaded for gapless ({} bytes)",
                data.len()
            );
            self.audio_cache.insert(track_id, data.clone());
        } else {
            log::info!("[GAPLESS] Track {track_id} not available, gapless not possible");
        }
        downloaded
    }

    /// Resolve a fully-materialized audio asset for an EXTERNAL renderer
    /// (Chromecast / DLNA), carrying the bytes verbatim plus the MIME and the
    /// quality. Used by the Cast path through the local media server.
    ///
    /// Cache-first (P1, matches the fast Tauri cast path + consumes the gapless
    /// prefetch): L1 in-memory -> L2 on-disk playback cache (both decrypted
    /// FLAC) -> network. A prefetched/replayed track is served instantly; only a
    /// cold track pays the CMAF download. On a cache hit the delivered quality is
    /// not known here (no metadata stored with the bytes) — the caller derives
    /// the quality label from the track's catalog metadata; the network path
    /// returns the precise resolved tier.
    /// Open a Qobuz track as a PROGRESSIVE byte source for an external
    /// renderer (Chromecast / DLNA): the CMAF download starts immediately,
    /// the returned buffer exposes the bytes as they land, and the exact
    /// final FLAC size is known up front (header + segment table) so the
    /// media server can advertise a truthful Content-Length. Returns once the
    /// initial buffer (first segments) is in, i.e. within ~0.3-1 s instead of
    /// after the whole file (5-6 s for a 100 MB Hi-Res track on a fast line,
    /// measured 2026-08-30). The finished download is cached like a played
    /// stream, so a replay / re-cast is served from memory.
    ///
    /// Cached tracks should NOT come through here — `fetch_for_external_stream`
    /// answers them from L1/L2 with no network at all.
    pub async fn open_external_stream(
        &self,
        client: &QobuzClient,
        track_id: u64,
        quality: Quality,
    ) -> Result<ExternalStreamHandle, String> {
        let cmaf_info = qbz_qobuz::cmaf::setup_streaming(client, track_id, quality).await?;
        let total_bytes = cmaf_info.flac_header.len() as u64
            + cmaf_info
                .segment_table
                .iter()
                .map(|segment| segment.byte_len as u64)
                .sum::<u64>();
        let (source, writer) =
            BufferedMediaSource::new(StreamingConfig::fast_start(), Some(total_bytes));
        let source = Arc::new(source);
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

        let url_template = cmaf_info.url_template;
        let content_key = cmaf_info.content_key;
        let flac_header = cmaf_info.flac_header;
        let n_segments = cmaf_info.n_segments;
        let cache = self.audio_cache.clone();
        tokio::spawn(async move {
            match Self::cmaf_stream_segments(
                &url_template,
                n_segments,
                content_key,
                flac_header,
                writer,
                track_id,
                cache,
                false,
                total_bytes,
                cancel_rx,
            )
            .await
            {
                Ok(true) => log::info!("[CAST-STREAM] Track {track_id} fully downloaded"),
                Ok(false) => {}
                Err(error) => log::warn!("[CAST-STREAM] Track {track_id} feeder failed: {error}"),
            }
        });

        // Hand the source over only once the renderer's first read can be
        // answered without stalling on the network (header + first segment).
        const INITIAL_BUFFER_TIMEOUT: Duration = Duration::from_secs(8);
        let waiter = Arc::clone(&source);
        let ready = tokio::time::timeout(
            INITIAL_BUFFER_TIMEOUT,
            tokio::task::spawn_blocking(move || waiter.wait_for_initial_buffer()),
        )
        .await;
        match ready {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => {
                let _ = cancel_tx.send(true);
                return Err(format!("cast stream download failed: {error}"));
            }
            Ok(Err(join)) => {
                let _ = cancel_tx.send(true);
                return Err(format!("cast stream waiter failed: {join}"));
            }
            Err(_) => {
                let _ = cancel_tx.send(true);
                return Err("cast stream initial buffer timed out".to_string());
            }
        }
        log::info!(
            "[CAST-STREAM] Track {track_id} progressive source ready ({} B buffered of {total_bytes})",
            source.buffer_size()
        );
        Ok(ExternalStreamHandle {
            source,
            total_bytes,
            sample_rate: cmaf_info.sampling_rate,
            bit_depth: cmaf_info.bit_depth,
            cancel: cancel_tx,
        })
    }

    pub async fn fetch_for_external_stream(
        &self,
        client: &QobuzClient,
        track_id: u64,
        quality: Quality,
    ) -> Option<ExternalStreamAsset> {
        // L1: in-memory cache (warmed by the gapless prefetch / a prior play).
        if let Some(cached) = self.audio_cache.get(track_id) {
            log::info!(
                "[CAST-FETCH] Track {track_id} from MEMORY cache ({} bytes)",
                cached.size_bytes
            );
            return Some(ExternalStreamAsset {
                bytes: cached.data,
                content_type: "audio/flac".to_string(),
                quality: StreamQualityInfo::from_raw(0, None, None),
                duration_secs: None,
                origin: AssetOrigin::Cache,
            });
        }
        // L2: on-disk plain-FLAC playback cache; warm L1 on the way out.
        if let Some(playback_cache) = self.audio_cache.get_playback_cache() {
            if let Some(audio_data) = playback_cache.get(track_id) {
                log::info!(
                    "[CAST-FETCH] Track {track_id} from DISK cache ({} bytes)",
                    audio_data.len()
                );
                self.audio_cache.promote_from_disk(track_id, audio_data.clone());
                return Some(ExternalStreamAsset {
                    bytes: audio_data,
                    content_type: "audio/flac".to_string(),
                    quality: StreamQualityInfo::from_raw(0, None, None),
                    duration_secs: None,
                    origin: AssetOrigin::Cache,
                });
            }
        }

        // Cold: CMAF full download (Akamai CDN) -> decrypted FLAC.
        match qbz_qobuz::cmaf::download_full_with_quality(client, track_id, quality).await {
            Ok((bytes, q)) => {
                log::info!(
                    "[CAST-FETCH] Track {track_id} via CMAF: {} bytes, format_id={}, {:?} kHz/{:?}-bit",
                    bytes.len(),
                    q.format_id,
                    q.sampling_rate_khz,
                    q.bit_depth
                );
                // Warm L1 so a subsequent local replay skips the network.
                self.audio_cache.insert(track_id, bytes.clone());
                return Some(ExternalStreamAsset {
                    bytes,
                    content_type: "audio/flac".to_string(),
                    quality: q,
                    duration_secs: None,
                    origin: AssetOrigin::Network,
                });
            }
            Err(e) => {
                log::warn!("[CAST-FETCH] CMAF failed for track {track_id}: {e}, trying legacy");
            }
        }

        // Fallback: legacy stream URL + plain HTTP download. Quality and MIME
        // come from the resolved StreamUrl (which carries the granted tier).
        match client.get_stream_url_with_fallback(track_id, quality).await {
            Ok(stream_url) => {
                let content_type =
                    external_content_type(&stream_url.mime_type, stream_url.format_id);
                let q = StreamQualityInfo::from_raw(
                    stream_url.format_id,
                    Some(stream_url.sampling_rate),
                    stream_url.bit_depth,
                );
                match self.download_audio(&stream_url.url).await {
                    Ok(bytes) => {
                        log::info!(
                            "[CAST-FETCH] Track {track_id} via legacy: {} bytes, format_id={}, ct={}",
                            bytes.len(),
                            q.format_id,
                            content_type
                        );
                        self.audio_cache.insert(track_id, bytes.clone());
                        Some(ExternalStreamAsset {
                            bytes,
                            content_type,
                            quality: q,
                            duration_secs: None,
                            origin: AssetOrigin::Network,
                        })
                    }
                    Err(e) => {
                        log::warn!("[CAST-FETCH] Legacy download failed for {track_id}: {e}");
                        None
                    }
                }
            }
            Err(e) => {
                log::warn!("[CAST-FETCH] No stream URL for {track_id}: {e}");
                None
            }
        }
    }

    /// Stream CMAF segments to the player's buffer, decrypting on the fly.
    ///
    /// Writes the FLAC header first so the decoder can identify the format,
    /// then fetches each audio segment, decrypts encrypted frames, and pushes
    /// the resulting FLAC frame data to the streaming buffer. The player
    /// starts playing as soon as enough data is buffered.
    ///
    /// Completed RAM buffers are shared; disk spools are persisted in bounded
    /// chunks outside the decoder lock, without a parallel RAM accumulator.
    ///
    /// `cancel_rx` flips to `true` when a newer play intent supersedes this
    /// stream (see `SharedState::cancel_stream_feeder`). Returns `Ok(true)`
    /// on real completion, `Ok(false)` when cancelled.
    async fn cmaf_stream_segments(
        url_template: &str,
        n_segments: u8,
        content_key: [u8; 16],
        flac_header: Vec<u8>,
        writer: BufferWriter,
        track_id: u64,
        cache: Arc<qbz_cache::AudioCache>,
        skip_cache: bool,
        _expected_total_bytes: u64,
        mut cancel_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Result<bool, String> {
        struct FailGuard {
            writer: BufferWriter,
            armed: bool,
        }
        impl Drop for FailGuard {
            fn drop(&mut self) {
                if self.armed {
                    let _ = self
                        .writer
                        .error("CMAF stream aborted before completion".into());
                }
            }
        }
        let mut guard = FailGuard {
            writer,
            armed: true,
        };
        let writer = &guard.writer;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("CMAF client error: {}", e))?;

        // Write the FLAC header first so the decoder can identify the format.
        if let Err(e) = writer.push_chunk(&flac_header) {
            let msg = format!("Failed to write FLAC header to buffer: {e}");
            let _ = writer.error(msg.clone());
            return Err(msg);
        }

        let mut total_written: u64 = flac_header.len() as u64;
        let start = Instant::now();

        for seg_idx in 1..=n_segments {
            let seg_url = url_template.replace("$SEGMENT$", &seg_idx.to_string());
            let fetch_tag = format!("CMAF-STREAM track {track_id} seg {seg_idx}");
            let fetch = qbz_qobuz::cmaf::fetch_cdn_bytes_with_retry(
                &client,
                &seg_url,
                &fetch_tag,
            );
            let seg_data = tokio::select! {
                biased;
                // Superseded by a newer play (or stop): drop the in-flight
                // request so the link frees immediately, and leave the
                // abandoned buffer OPEN (no error, no complete) so the
                // outgoing engine keeps playing what is already buffered
                // until the audio thread installs the new source; the new
                // play seals the buffer after the swap. The partial track is
                // NOT cached — the user abandoned it.
                _ = cancel_rx.changed() => {
                    log::info!(
                        "[CMAF-STREAM] Track {} feeder cancelled (superseded)",
                        track_id
                    );
                    guard.armed = false;
                    return Ok(false);
                }
                data = fetch => data?,
            };

            let crypto = qbz_cmaf::parse_segment_crypto(&seg_data)
                .map_err(|e| format!("CMAF segment {} parse: {}", seg_idx, e))?;

            let mut data_pos = crypto.data_offset;
            for entry in &crypto.entries {
                let frame_end = data_pos + entry.size as usize;
                if frame_end > seg_data.len() {
                    let _ = writer.error(format!("CMAF segment {} frame overflow", seg_idx));
                    return Err(format!("CMAF segment {} frame overflow", seg_idx));
                }
                let mut frame = seg_data[data_pos..frame_end].to_vec();
                if entry.flags != 0 {
                    qbz_cmaf::decrypt_frame(&content_key, &entry.iv, &mut frame);
                }
                // Write decrypted frame to the streaming buffer.
                if let Err(e) = writer.push_chunk(&frame) {
                    let msg = format!("Failed to push frame: {e}");
                    let _ = writer.error(msg.clone());
                    return Err(msg);
                }
                total_written += frame.len() as u64;
                data_pos = frame_end;
            }

            // Trailing unencrypted data after all frame entries.
            if data_pos < crypto.mdat_end && crypto.mdat_end <= seg_data.len() {
                let trailing = &seg_data[data_pos..crypto.mdat_end];
                if let Err(e) = writer.push_chunk(trailing) {
                    let msg = format!("Failed to push trailing data: {e}");
                    let _ = writer.error(msg.clone());
                    return Err(msg);
                }
                total_written += trailing.len() as u64;
            }

            // Progress logging every 5 segments or on the last segment.
            if seg_idx % 5 == 0 || seg_idx == n_segments {
                let elapsed = start.elapsed().as_secs_f64();
                let mbps = if elapsed > 0.0 {
                    total_written as f64 / (1024.0 * 1024.0) / elapsed
                } else {
                    0.0
                };
                // Feed the adaptive prefetch throttle with the cumulative MB/s
                // the log already reports; also the offline detector's
                // positive liveness signal (#467, #591).
                qbz_audio::network_throttle::state().record_segment_bandwidth(mbps);
                log::info!(
                    "[CMAF-STREAM] Segment {}/{} ({:.1} MB, {:.1} MB/s)",
                    seg_idx,
                    n_segments - 1,
                    total_written as f64 / (1024.0 * 1024.0),
                    mbps
                );
            }
        }

        // Signal end of stream (disarm fail-guard so Drop does not error).
        guard.armed = false;
        if let Err(e) = writer.complete() {
            log::error!("[CMAF-STREAM] Failed to mark buffer complete: {}", e);
            let _ = writer.error(format!("Failed to mark buffer complete: {e}"));
            return Err(format!("Failed to mark buffer complete: {e}"));
        }

        log::info!(
            "[CMAF-STREAM] Complete: {:.2} MB written in {:.1}s for track {}, segments fetched: 1..{}",
            total_written as f64 / (1024.0 * 1024.0),
            start.elapsed().as_secs_f64(),
            track_id,
            n_segments - 1
        );

        if !skip_cache {
            Self::persist_stream(cache, writer.clone(), track_id).await?;
        }

        Ok(true)
    }

    /// Play from raw audio data (for cached tracks)
    /// Play from raw audio data (for cached / offline tracks).
    ///
    /// Bumps play generation so this call supersedes any in-flight
    /// `play_track` that has not yet applied audio.
    pub fn play_data(&self, data: Vec<u8>, track_id: u64) -> Result<(), String> {
        let _gen = self.begin_play();
        self.apply_play_data(data, track_id)
    }

    /// Send `Play` without bumping generation (used by `play_track` after
    /// its own `begin_play` + supersede checks).
    fn apply_play_data(&self, data: Vec<u8>, track_id: u64) -> Result<(), String> {
        log::info!(
            "Player: Playing {} bytes of audio data for track {}",
            data.len(),
            track_id
        );

        let play_gen = self.state.current_play_generation();
        self.state.begin_buffering(track_id, play_gen);

        // Extract audio metadata (sample rate, channels, bit depth) - fast header-only read
        let meta = extract_audio_metadata_full(&data).map_err(|e| {
            self.state.set_buffer_state_for_play(
                PlaybackBufferState::Error,
                track_id,
                play_gen,
            );
            format!("Failed to extract audio metadata: {}", e)
        })?;

        let sample_rate = meta.sample_rate;
        let channels = meta.channels;
        let bit_depth = meta.bit_depth.unwrap_or(16);

        log::info!(
            "Player: Detected audio format - {}Hz, {} channels, {}-bit",
            sample_rate,
            channels,
            bit_depth
        );

        // Update shared state with actual stream quality
        self.state.set_stream_quality(sample_rate, bit_depth);

        self.tx
            .send(AudioCommand::Play {
                data,
                track_id,
                duration_secs: 0, // Will be determined by decoder
                sample_rate,
                channels,
                play_gen,
            })
            .map_err(|e| {
                self.state.set_buffer_state_for_play(
                    PlaybackBufferState::Error,
                    track_id,
                    play_gen,
                );
                log::error!("Player: Failed to send to audio thread: {}", e);
                format!(
                    "Failed to send play command (audio thread may have crashed): {}",
                    e
                )
            })?;

        // A new source was just installed: seal the abandoned feeder's
        // buffer so a parked reader wakes to EOF instead of hanging.
        self.state.seal_stream_feeder();

        log::info!("Player: Playback initiated successfully");
        Ok(())
    }

    /// Queue next track for gapless playback (appends to current Sink without stopping)
    pub fn play_next(&self, data: Vec<u8>, track_id: u64) -> Result<(), String> {
        let meta = extract_audio_metadata_full(&data)
            .map_err(|e| format!("Failed to extract audio metadata for gapless: {}", e))?;

        log::info!(
            "Player: Queueing gapless track {} ({}Hz, {}ch, {} bytes)",
            track_id,
            meta.sample_rate,
            meta.channels,
            data.len()
        );

        self.tx
            .send(AudioCommand::PlayNext {
                data,
                track_id,
                sample_rate: meta.sample_rate,
                channels: meta.channels,
                bit_depth: meta.bit_depth.unwrap_or(16),
            })
            .map_err(|e| {
                log::error!("Player: Failed to send PlayNext to audio thread: {}", e);
                format!("Failed to send gapless command: {}", e)
            })
    }

    /// Play a local DSD file (.dsf/.dff) by converting it on the fly to
    /// 176.4 kHz / 24-bit PCM (qbz-dsd, Phase 1 of the DSD plan — see
    /// qbz-nix-docs/dsd-support/). The converted stream rides the existing
    /// `play_streaming` path as an ordinary finite WAV: a background thread
    /// demuxes + decimates and pushes into the BufferWriter, so the whole
    /// PCM pipeline (engines, bit-perfect ALSA at 176.4 kHz, volume,
    /// normalization, seek-in-buffer) behaves exactly as for any hi-res
    /// track. DST-compressed DFF and >2ch files are rejected with a
    /// readable error before anything starts.
    pub fn play_dsd_file(&self, path: std::path::PathBuf, track_id: u64) -> Result<(), String> {
        let _gen = self.begin_play();
        let demux = qbz_dsd::open_dsd(&path).map_err(|e| e.to_string())?;
        let dsd_rate = demux.info().dsd_rate;

        // DoP resolution (Phase 2): user opt-in + ALSA direct backend +
        // stereo + carrier rate supported by the device. Anything else falls
        // through to the universal DSD→PCM conversion below.
        #[cfg(target_os = "linux")]
        {
            let info = demux.info().clone();
            let resolved = self
                .audio_settings
                .lock()
                .ok()
                .map(|s| {
                    (
                        s.dsd_mode.clone(),
                        matches!(s.backend_type, Some(qbz_audio::AudioBackendType::Alsa)),
                        s.output_device.clone(),
                    )
                })
                .unwrap_or(("convert".to_string(), false, None));
            if let (mode, true, Some(device)) = resolved {
                if info.channels == 2 && mode != "convert" {
                    if mode == "native" {
                        // The open itself validates (kernel quirk / rates);
                        // failure surfaces as a stream error toast.
                        log::info!(
                            "Player: DSD track {} — {} via NATIVE DSD",
                            track_id,
                            qbz_dsd::dsd_label(info.dsd_rate)
                        );
                        drop(demux);
                        return self
                            .tx
                            .send(AudioCommand::PlayDsdNative { path, track_id })
                            .map_err(|e| {
                                format!("Failed to send native DSD play command: {}", e)
                            });
                    }
                    let carrier = qbz_dsd::dop_carrier_rate(info.dsd_rate);
                    // Ask the platform that owns the device. On Windows the
                    // ALSA probe cannot answer, so this guard was taking its
                    // `.unwrap_or(true)` and dispatching DoP on a DEFAULT
                    // rather than on a measurement -- on a DAC that refuses
                    // 176400, which is the DSD64 carrier.
                    // WINDOWS: never. Not "probe and decide" -- the DoP
                    // handler below is `cfg(target_os = "linux")` and answers
                    // "DoP playback is Linux-only" with a stream error, so
                    // dispatching here can only ever fail. Converting to PCM
                    // is the one outcome that plays the track.
                    //
                    // It is also FAIL-CLOSED, which this guard has to be. An
                    // undecoded DoP stream is read as ordinary PCM carrying
                    // packed DSD and marker bytes: it comes out as very loud
                    // broadband noise, which threatens tweeters and hearing.
                    // An earlier draft here fell back to `true` whenever the
                    // probe could not answer -- preserving exactly the guess
                    // this work existed to remove.
                    //
                    // `wasapi_backend::supported_rates` is what a future
                    // Windows DoP path would consult, and it already reports
                    // that the owner's DAC refuses 176400, the DSD64 carrier.
                    #[cfg(windows)]
                    let rate_ok = false;
                    #[cfg(not(windows))]
                    let rate_ok =
                        qbz_audio::alsa_backend::get_device_supported_rates(&device)
                            .map(|r| r.contains(&carrier))
                            .unwrap_or(true);
                    if rate_ok {
                        log::info!(
                            "Player: DSD track {} — {} via DoP ({} Hz carrier)",
                            track_id,
                            qbz_dsd::dsd_label(info.dsd_rate),
                            carrier
                        );
                        drop(demux);
                        return self
                            .tx
                            .send(AudioCommand::PlayDsdDop { path, track_id })
                            .map_err(|e| format!("Failed to send DoP play command: {}", e));
                    }
                    log::info!(
                        "Player: DoP selected but device lacks the {} Hz carrier — converting to PCM",
                        carrier
                    );
                } else if info.channels != 2 && mode != "convert" {
                    log::info!(
                        "Player: {} selected but track has {} channels — downmix-converting to PCM",
                        mode,
                        info.channels
                    );
                }
            }
        }
        let mut conv = qbz_dsd::DsdPcmConverter::new(demux, qbz_dsd::DEFAULT_GAIN_DB)
            .map_err(|e| e.to_string())?;
        let channels = conv.channels();
        let rate = conv.output_rate();
        let total_frames = conv.total_frames();
        let duration_secs = total_frames / rate as u64;
        let content_length = qbz_dsd::wav_total_size(total_frames, channels);
        log::info!(
            "Player: DSD track {} — {} ({} Hz) → PCM {} Hz/24-bit, {}s, {} bytes WAV",
            track_id,
            qbz_dsd::dsd_label(dsd_rate),
            dsd_rate,
            rate,
            duration_secs,
            content_length
        );
        self.state.set_stream_quality(rate, 24);

        let writer = self.apply_play_streaming(
            track_id,
            rate,
            channels,
            content_length,
            3,
            duration_secs,
            0,
        )?;

        // Cancel-on-supersede, the same contract `register_stream_feeder`
        // gives the CMAF feeder. This loop USED to rely on `push_chunk`
        // returning `Err` to notice the track had changed — but `push_chunk`
        // only extends a `Vec` and fails solely on a poisoned mutex
        // (`streaming_source.rs:541`), and `BufferState` carries no
        // reader-gone flag, so that check could never fire. Skipping a DSD
        // track therefore left this thread converting to the END of the file,
        // growing a buffer no reader would ever read: a 60-minute DSD64 album
        // track is ~1.9 GB of 88.2 kHz/24-bit PCM held for nothing.
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let slot_writer = writer.clone();
        let play_gen = self.state.current_play_generation();

        std::thread::spawn(move || {
            if writer
                .push_chunk(&qbz_dsd::wav_header(total_frames, channels, rate))
                .is_err()
            {
                return;
            }
            let mut pcm = Vec::new();
            loop {
                // Cooperative, checked per block (~a fraction of a second of
                // audio), so a skip stops the conversion promptly. Like the
                // CMAF feeder's own cancel, this deliberately neither errors
                // nor completes the buffer: the outgoing engine keeps playing
                // what is already buffered until the new source is installed,
                // and `seal_stream_feeder` closes it from there.
                if *cancel_rx.borrow() {
                    log::info!(
                        "Player: DSD conversion for superseded track {} cancelled",
                        track_id
                    );
                    return;
                }
                match conv.next_block() {
                    Ok(Some(frames)) => {
                        pcm.clear();
                        qbz_dsd::frames_to_pcm24(&frames, &mut pcm);
                        if writer.push_chunk(&pcm).is_err() {
                            // Only a poisoned buffer lock reaches here; the
                            // track-changed case is the cancel check above.
                            return;
                        }
                    }
                    Ok(None) => {
                        let _ = writer.complete();
                        return;
                    }
                    Err(e) => {
                        log::error!("Player: DSD conversion failed mid-track: {}", e);
                        let _ = writer.error(format!("DSD conversion failed: {}", e));
                        return;
                    }
                }
            }
        });
        // After the spawn, and with the generation this play was born under:
        // a supersede that landed in between makes `register_stream_feeder`
        // cancel the thread instead of registering it.
        self.state
            .register_stream_feeder(track_id, cancel_tx, slot_writer, play_gen);
        Ok(())
    }

    /// Whole-file DSD→PCM conversion into an in-memory WAV, for the gapless
    /// prefetch path: the result feeds `play_next` like any cached track, so
    /// consecutive converted-DSD tracks hand off seamlessly. CPU-bound
    /// (~10-30x realtime) — call from a blocking context.
    pub fn prepare_dsd_gapless_wav(path: &std::path::Path) -> Result<Vec<u8>, String> {
        let demux = qbz_dsd::open_dsd(path).map_err(|e| e.to_string())?;
        let mut conv = qbz_dsd::DsdPcmConverter::new(demux, qbz_dsd::DEFAULT_GAIN_DB)
            .map_err(|e| e.to_string())?;
        let channels = conv.channels();
        let rate = conv.output_rate();
        let total = conv.total_frames();
        let mut out = qbz_dsd::wav_header(total, channels, rate);
        out.reserve(total as usize * channels as usize * 3);
        while let Some(frames) = conv.next_block().map_err(|e| e.to_string())? {
            qbz_dsd::frames_to_pcm24(&frames, &mut out);
        }
        Ok(out)
    }

    /// Queue the next DSD track for a gapless transition: appends to the DoP
    /// engine when one is active (seamless native DSD), otherwise converts to
    /// an in-memory WAV and rides the normal `play_next` gapless path.
    pub fn play_next_dsd(&self, path: std::path::PathBuf, track_id: u64) -> Result<(), String> {
        if self.state.is_dsd_direct() {
            return self
                .tx
                .send(AudioCommand::PlayNextDsdDop { path, track_id })
                .map_err(|e| format!("Failed to send DoP gapless command: {}", e));
        }
        let wav = Self::prepare_dsd_gapless_wav(&path)?;
        self.play_next(wav, track_id)
    }

    /// True while a DoP stream is active (volume fixed, seek unsupported).
    pub fn is_dsd_direct_active(&self) -> bool {
        self.state.is_dsd_direct()
    }

    /// Play from streaming source (starts playback before full download).
    /// Returns the BufferWriter so caller can push data as it downloads.
    /// `start_position_secs` > 0 turns this into a session-resume play
    /// (#315): the audio thread waits for enough buffer to cover the
    /// offset and pre-skips decoder output up to that point.
    pub fn play_streaming(
        &self,
        track_id: u64,
        sample_rate: u32,
        channels: u16,
        content_length: u64,
        buffer_seconds: u8,
        duration_secs: u64,
        start_position_secs: u64,
    ) -> Result<BufferWriter, String> {
        let _gen = self.begin_play();
        self.apply_play_streaming(
            track_id,
            sample_rate,
            channels,
            content_length,
            buffer_seconds,
            duration_secs,
            start_position_secs,
        )
    }

    /// `play_streaming` without bumping generation (used by play paths that
    /// already did their own `begin_play` + supersede checks, like
    /// `play_dsd_file`; a mid-intent bump here would invalidate a strictly
    /// newer play that started in the meantime).
    fn apply_play_streaming(
        &self,
        track_id: u64,
        sample_rate: u32,
        channels: u16,
        content_length: u64,
        buffer_seconds: u8,
        duration_secs: u64,
        start_position_secs: u64,
    ) -> Result<BufferWriter, String> {
        log::info!(
            "Player: Starting streaming playback for track {} ({}Hz, {}ch, {} bytes total, {}s, start={}s)",
            track_id,
            sample_rate,
            channels,
            content_length,
            duration_secs,
            start_position_secs
        );

        // Use StreamingConfig::from_seconds for proper buffer sizing
        let config = StreamingConfig::from_seconds(buffer_seconds);

        let (source, writer) = self.streaming_buffer(config, Some(content_length))?;
        let source = Arc::new(source);
        let play_gen = self.state.current_play_generation();
        self.state.begin_buffering(track_id, play_gen);

        self.tx
            .send(AudioCommand::PlayStreaming {
                source: source.clone(),
                track_id,
                sample_rate,
                channels,
                duration_secs,
                start_position_secs,
                content_length,
                play_gen,
            })
            .map_err(|e| {
                self.state.set_buffer_state_for_play(
                    PlaybackBufferState::Error,
                    track_id,
                    play_gen,
                );
                log::error!("Player: Failed to send streaming command: {}", e);
                format!("Failed to send streaming play command: {}", e)
            })?;

        // A new source was just installed: seal the abandoned feeder's
        // buffer so a parked reader wakes to EOF instead of hanging.
        self.state.seal_stream_feeder();

        log::info!("Player: Streaming playback initiated");
        Ok(writer)
    }

    /// Play from streaming source with dynamic buffer based on measured speed.
    /// `start_position_secs` > 0 signals session resume (see `play_streaming`).
    pub fn play_streaming_dynamic(
        &self,
        track_id: u64,
        sample_rate: u32,
        channels: u16,
        bit_depth: u32,
        content_length: u64,
        speed_mbps: f64,
        duration_secs: u64,
        start_position_secs: u64,
    ) -> Result<BufferWriter, String> {
        let _gen = self.begin_play();
        self.apply_play_streaming_dynamic(
            track_id,
            sample_rate,
            channels,
            bit_depth,
            content_length,
            speed_mbps,
            duration_secs,
            start_position_secs,
        )
    }

    /// `play_streaming_dynamic` without bumping generation (used by
    /// `play_track`'s CMAF path, which already holds a generation token; a
    /// mid-intent bump here would invalidate a strictly newer play that
    /// started in the meantime).
    fn apply_play_streaming_dynamic(
        &self,
        track_id: u64,
        sample_rate: u32,
        channels: u16,
        bit_depth: u32,
        content_length: u64,
        speed_mbps: f64,
        duration_secs: u64,
        start_position_secs: u64,
    ) -> Result<BufferWriter, String> {
        log::info!(
            "Player: Starting dynamic streaming for track {} ({}Hz, {}ch, {}-bit, {:.2} MB, {:.1} MB/s, {}s, start={}s)",
            track_id,
            sample_rate,
            channels,
            bit_depth,
            content_length as f64 / (1024.0 * 1024.0),
            speed_mbps,
            duration_secs,
            start_position_secs
        );

        // Update shared state with actual stream quality
        self.state.set_stream_quality(sample_rate, bit_depth);

        // Use StreamingConfig::from_speed_mbps for dynamic buffer sizing
        let mut config = StreamingConfig::from_speed_mbps(speed_mbps);

        // Floor the initial buffer at the user's `stream_buffer_seconds` worth
        // of REAL audio bytes (#591). content_length / duration is the track's
        // true average byterate (both derive from the CMAF segment table),
        // unlike `speed_mbps`, which is estimated from the tiny init fetch and
        // is latency-dominated — it lands on the slowest ladder rung for every
        // connection. Clamped to 256KB (format-detection minimum) .. 8MB (the
        // process-wide ladder cap is set from the host memory profile at
        // process start, so on a LowMemory host `from_speed_mbps` already
        // clamped lower; the 8MB ceiling here is the last line of defense).
        let user_secs = self
            .audio_settings
            .lock()
            .map(|s| s.stream_buffer_seconds)
            .unwrap_or(2);
        let bps = content_length / duration_secs.max(1);
        let floor = (user_secs as u64).saturating_mul(bps) as usize;
        let ladder_bytes = config.initial_buffer_bytes;
        config.initial_buffer_bytes = ladder_bytes
            .max(floor)
            .clamp(256 * 1024, 8 * 1024 * 1024);
        if config.initial_buffer_bytes != ladder_bytes {
            log::info!(
                "Dynamic buffer: user floor {}s x {} B/s → {}KB initial buffer (ladder gave {}KB)",
                user_secs,
                bps,
                config.initial_buffer_bytes / 1024,
                ladder_bytes / 1024
            );
        }

        let (source, writer) = self.streaming_buffer(config, Some(content_length))?;
        let source = Arc::new(source);
        let play_gen = self.state.current_play_generation();
        self.state.begin_buffering(track_id, play_gen);

        self.tx
            .send(AudioCommand::PlayStreaming {
                source: source.clone(),
                track_id,
                sample_rate,
                channels,
                duration_secs,
                start_position_secs,
                content_length,
                play_gen,
            })
            .map_err(|e| {
                self.state.set_buffer_state_for_play(
                    PlaybackBufferState::Error,
                    track_id,
                    play_gen,
                );
                log::error!("Player: Failed to send streaming command: {}", e);
                format!("Failed to send streaming play command: {}", e)
            })?;

        // A new source was just installed: seal the abandoned feeder's
        // buffer so a parked reader wakes to EOF instead of hanging.
        self.state.seal_stream_feeder();

        log::info!("Player: Dynamic streaming playback initiated");
        Ok(writer)
    }

    /// Download audio from URL with timeout
    async fn download_audio(&self, url: &str) -> Result<Vec<u8>, String> {
        use std::time::Duration;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "Failed to create HTTP client".to_string())?;

        log::info!("Caching audio from URL...");

        let response = client
            .get(url)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await
            .map_err(|e| format!(
                "Failed to fetch audio: {}",
                crate::remote_stream::describe_reqwest_error(&e)
            ))?;

        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()));
        }

        log::info!("Response received, reading bytes...");

        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!(
                "Failed to read audio bytes: {}",
                crate::remote_stream::describe_reqwest_error(&e)
            ))?;

        log::info!("Cached {} bytes", bytes.len());
        Ok(bytes.to_vec())
    }

    /// Pause playback
    pub fn pause(&self) -> Result<(), String> {
        self.tx
            .send(AudioCommand::Pause)
            .map_err(|e| format!("Failed to send pause command: {}", e))
    }

    /// Resume playback
    pub fn resume(&self) -> Result<(), String> {
        self.tx
            .send(AudioCommand::Resume)
            .map_err(|e| format!("Failed to send resume command: {}", e))
    }

    pub fn has_loaded_audio(&self) -> bool {
        self.state.has_loaded_audio()
    }

    /// Stop playback
    pub fn stop(&self) -> Result<(), String> {
        // Supersede any in-flight play_track so a slow CMAF/legacy fetch cannot
        // restart audio after the user stopped.
        let _ = self.begin_play();
        // Nothing will consume the abandoned stream's buffer after this:
        // seal it so a reader parked on its condvar wakes to EOF.
        self.state.seal_stream_feeder();
        self.tx
            .send(AudioCommand::Stop { completed: None })
            .map_err(|e| format!("Failed to send stop command: {}", e))
    }

    /// Stop and wait until the audio thread has dropped current/gapless
    /// readers before evicting L1. Callers first cancel speculative downloads.
    /// Keep L2 and the selected memory policy; the next start builds new buffers.
    pub async fn stop_and_evict_playback_memory(&self) -> Result<(), String> {
        let _ = self.begin_play();
        self.state.seal_stream_feeder();
        let (completed, receiver) = tokio::sync::oneshot::channel();
        self.tx.send(AudioCommand::Stop { completed: Some(completed) })
            .map_err(|e| format!("Failed to send stop command: {e}"))?;
        tokio::time::timeout(Duration::from_secs(10), receiver).await
            .map_err(|_| "Timed out waiting for playback memory release".to_string())?
            .map_err(|_| "Audio thread closed before playback memory release".to_string())?;
        self.audio_cache.evict_all_memory();
        Ok(())
    }

    /// Set volume (0.0 - 1.0)
    pub fn set_volume(&self, volume: f32) -> Result<(), String> {
        let clamped = volume.clamp(0.0, 1.0);

        // Skip if volume is already at this value (prevents MPRIS/PipeWire feedback loop)
        let current = self.state.volume();
        if (clamped - current).abs() < 0.001 {
            return Ok(());
        }

        self.tx
            .send(AudioCommand::SetVolume(clamped))
            .map_err(|e| format!("Failed to send volume command: {}", e))
    }

    /// Seed the volume state for an imminent engine rebuild without touching
    /// the currently open output. Settings uses this when enabling ALSA
    /// hardware volume: it samples the physical mixer first, stores that level
    /// here, then reinitializes the direct engine. Sending `SetVolume` instead
    /// would write the new device's sampled level through the *old* device's
    /// still-live hardware mixer before the reinit command reached the audio
    /// thread.
    pub fn seed_volume_state(&self, volume: f32) {
        self.state
            .volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::SeqCst);
    }

    /// Seek to position in seconds
    pub fn seek(&self, position: u64) -> Result<(), String> {
        // Clamp to duration if known
        let duration = self.state.duration();
        let clamped_position = if duration > 0 {
            position.min(duration)
        } else {
            position
        };

        self.tx
            .send(AudioCommand::Seek(clamped_position))
            .map_err(|e| format!("Failed to send seek command: {}", e))
    }

    /// Reinitialize audio device (releases and re-acquires the device)
    /// Use this when changing audio settings like exclusive mode
    pub fn reinit_device(&self, device_name: Option<String>) -> Result<(), String> {
        self.tx
            .send(AudioCommand::ReinitDevice { device_name })
            .map_err(|e| format!("Failed to send reinit command: {}", e))
    }

    /// Release the output device without reopening it. Drops the active
    /// stream — freeing an exclusive ALSA `hw:` grab and its D-Bus
    /// reservation — and un-suspends / un-forces anything QBZ parked, so
    /// PipeWire/WirePlumber can reclaim a device QBZ was holding (e.g. a DAC
    /// left invisible to other apps after bit-perfect ALSA Direct). Pair
    /// with a device re-enumeration in the UI to surface a freed or
    /// hot-plugged DAC without restarting the app.
    pub fn release_device(&self) -> Result<(), String> {
        let (completed_tx, completed_rx) = mpsc::sync_channel(1);
        self.tx
            .send(AudioCommand::ReleaseDevice {
                completed: completed_tx,
            })
            .map_err(|e| format!("Failed to send release command: {}", e))?;
        completed_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| format!("Timed out waiting for audio-device release: {error}"))?
    }

    /// Reload audio settings from fresh config (e.g., after database update)
    /// Call this before reinit_device() to ensure Player uses latest settings
    pub fn playback_cache_stats(&self) -> qbz_cache::CacheStats {
        self.audio_cache.stats()
    }

    pub fn playback_storage_directory(&self) -> Option<std::path::PathBuf> {
        self.audio_cache.get_playback_cache().map(|cache| cache.cache_dir().clone())
    }

    pub fn playback_prefetch_policy(&self) -> qbz_models::playback_cache::PlaybackPrefetchPolicy {
        let policy = self.audio_settings.lock().map(|s| s.playback_cache.clone()).unwrap_or_default();
        policy.prefetch_policy(self.audio_cache.stats().recommended_size_bytes)
    }

    /// Update only memory policy; never apply an unrelated pending output/DSD setting.
    pub fn configure_playback_cache(
        &self,
        policy: qbz_models::playback_cache::PlaybackCacheSettings,
    ) -> Result<(), String> {
        let mut settings = self.audio_settings.lock().map_err(|_| "Failed to lock audio settings")?;
        self.audio_cache.configure(policy.clone())?;
        set_max_initial_buffer_bytes(policy.prefetch_policy(self.audio_cache.stats().recommended_size_bytes).initial_buffer_cap);
        settings.playback_cache = policy;
        Ok(())
    }

    pub fn reload_settings(&self, settings: AudioSettings) -> Result<(), String> {
        self.audio_cache.configure(settings.playback_cache.clone())?;
        set_max_initial_buffer_bytes(settings.playback_cache.prefetch_policy(self.audio_cache.stats().recommended_size_bytes).initial_buffer_cap);
        if let Ok(mut current_settings) = self.audio_settings.lock() {
            *current_settings = settings;
            Ok(())
        } else {
            Err("Failed to lock audio settings".to_string())
        }
    }

    /// Get current playback state with real-time position
    pub fn get_state(&self) -> Result<PlaybackState, String> {
        Ok(PlaybackState {
            is_playing: self.state.is_playing(),
            position: self.state.current_position(),
            duration: self.state.duration(),
            track_id: self.state.current_track_id(),
            volume: self.state.volume(),
        })
    }

    /// Get playback event for emitting to frontend
    pub fn get_playback_event(&self) -> PlaybackEvent {
        let sample_rate = self.state.get_sample_rate();
        let bit_depth = self.state.get_bit_depth();
        let (buffer_state, buffer_track_id) = self.state.playback_buffer_snapshot();
        PlaybackEvent {
            is_playing: self.state.is_playing(),
            position: self.state.current_position(),
            duration: self.state.duration(),
            track_id: self.state.current_track_id(),
            volume: self.state.volume(),
            hardware_volume_active: self.state.hardware_volume_active(),
            sample_rate: if sample_rate > 0 {
                Some(sample_rate)
            } else {
                None
            },
            bit_depth: if bit_depth > 0 { Some(bit_depth) } else { None },
            shuffle: None, // Set by caller with access to queue state
            repeat: None,  // Set by caller with access to queue state
            normalization_gain: self.state.get_normalization_gain(),
            gapless_ready: self.state.is_gapless_ready(),
            gapless_next_track_id: self.state.get_gapless_next_track_id(),
            bit_perfect_mode: self.state.get_bit_perfect_mode(),
            buffer_progress: self.state.get_buffer_progress(),
            buffer_state,
            buffer_track_id,
            engine_empty_generation: self
                .state
                .engine_empty_generation
                .load(Ordering::SeqCst),
            engine_empty_track_id: self.state.engine_empty_track_id.load(Ordering::SeqCst),
            source_failure_generation: self
                .state
                .source_failure_generation
                .load(Ordering::SeqCst),
            source_failure_track_id: self.state.source_failure_track_id.load(Ordering::SeqCst),
        }
    }
}

/// Playback state snapshot
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PlaybackState {
    pub is_playing: bool,
    pub position: u64,
    pub duration: u64,
    pub track_id: u64,
    pub volume: f32,
}

/// Surfaces a DSD stream's mid-file demux I/O error as a stream error when
/// the stream runs out. The DoP writer consumes these streams as boxed
/// `Iterator<Item = i32>`, which can only say "no more words", so without
/// this wrapper a read failure is indistinguishable from a clean end of
/// track and the user gets a silent early stop.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct DsdErrorReport<S: qbz_dsd::DsdWordSource> {
    inner: S,
    // SharedState is a #[derive(Clone)] handle of Arc<Atomic..> fields, so a
    // clone already shares the same state — no extra Arc wrapper (that is also
    // the type `thread_state` is passed around as at the call sites).
    state: SharedState,
    reported: bool,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl<S: qbz_dsd::DsdWordSource> DsdErrorReport<S> {
    fn new(inner: S, state: SharedState) -> Self {
        Self {
            inner,
            state,
            reported: false,
        }
    }
}

impl<S: qbz_dsd::DsdWordSource> Iterator for DsdErrorReport<S> {
    type Item = i32;
    fn next(&mut self) -> Option<i32> {
        let item = self.inner.next();
        if item.is_none() && !self.reported {
            self.reported = true;
            if let Some(e) = self.inner.io_error() {
                self.state
                    .record_stream_error(format!("DSD playback failed: {e}"));
            }
        }
        item
    }
}

/// Pick the MIME to advertise to an external renderer for a legacy stream-URL
/// download. Prefer the server-provided `mime_type`; when it is empty (Qobuz
/// can return `""`), fall back by format id so the renderer is never handed an
/// empty content type (which some Chromecast/DLNA renderers reject).
pub fn external_content_type(mime: &str, format_id: u32) -> String {
    let trimmed = mime.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    match qbz_models::Quality::from_id(format_id) {
        Some(qbz_models::Quality::Mp3) => "audio/mpeg".to_string(),
        // Lossless / HiRes / UltraHiRes are FLAC over the file/url path.
        Some(_) => "audio/flac".to_string(),
        None => "audio/flac".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::compute_needs_new_stream;
    use super::{wedge_recovery, WedgeRecovery, WEDGE_REBUILD_BACKOFF};
    use super::external_content_type;
    #[cfg(target_os = "linux")]
    use super::{hardware_volume_event_callback, reported_volume_after_command};
    use super::{
        BufferedMediaSource, DsdErrorReport, PlaybackBufferReporter, PlaybackBufferState,
        SharedState, StreamingConfig,
    };
    #[cfg(target_os = "linux")]
    use qbz_audio::alsa_hardware_volume::{
        AlsaMixerControlId, HardwareVolumeEvent, HardwareVolumeSnapshot,
    };
    use std::sync::atomic::Ordering;

    #[test]
    fn duplicate_resume_preserves_live_clock_and_real_pause_can_resume() {
        let state = SharedState::new();
        state.duration.store(290, Ordering::SeqCst);
        state.start_playback_timer(67);
        state.is_playing.store(true, Ordering::SeqCst);
        let started = state.playback_start_millis.load(Ordering::SeqCst);
        assert_eq!(
            state.position(),
            0,
            "stored pause position intentionally stale"
        );
        state.resume_playback_timer();
        assert_eq!(state.playback_start_millis.load(Ordering::SeqCst), started);
        assert_eq!(state.position_at_start.load(Ordering::SeqCst), 67);
        assert!(state.current_position() >= 67);
        state.pause_playback_timer();
        state.is_playing.store(false, Ordering::SeqCst);
        let paused = state.position();
        state.resume_playback_timer();
        assert_eq!(state.position_at_start.load(Ordering::SeqCst), paused);
        assert_ne!(state.playback_start_millis.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn output_telemetry_is_independent_of_source_quality_and_clears_on_release() {
        let state = SharedState::new();
        state.set_stream_quality(192000, 24);
        assert_eq!(state.output_stream_format(), None);
        state
            .output_stream_format
            .store(48000 | (2_u64 << 32), Ordering::Release);
        assert_eq!(state.output_stream_format(), Some((48000, 2)));
        assert_eq!(state.get_sample_rate(), 192000);
        state.publish_output_stream_format(None);
        assert_eq!(state.output_stream_format(), None);
    }

    #[test]
    #[ignore = "requires QBZ_ALAC_PROBE_PATH to a real ALAC file"]
    fn real_alac_metadata_matches_decoded_signal() {
        use symphonia::core::io::MediaSourceStream;
        let path = std::env::var("QBZ_ALAC_PROBE_PATH").expect("real ALAC file path");
        let bytes = std::fs::read(path).unwrap();
        let meta = super::extract_audio_metadata_full(&bytes).unwrap();
        let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
        let mut hint = symphonia::core::probe::Hint::new();
        hint.with_extension("m4a");
        let mut format = symphonia::default::get_probe()
            .format(&hint, mss, &Default::default(), &Default::default())
            .unwrap()
            .format;
        let track = format.default_track().unwrap();
        let id = track.id;
        let cookie = track.codec_params.extra_data.as_ref().unwrap();
        let bits = cookie[5] as u32;
        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &Default::default())
            .unwrap();
        loop {
            let packet = format.next_packet().unwrap();
            if packet.track_id() != id {
                continue;
            }
            let audio = decoder.decode(&packet).unwrap();
            eprintln!(
                "metadata={}Hz/{:?}bit/{}ch, decoded={}Hz/{}bit/{}ch",
                meta.sample_rate,
                meta.bit_depth,
                meta.channels,
                audio.spec().rate,
                bits,
                audio.spec().channels.count()
            );
            assert_eq!(meta.sample_rate, audio.spec().rate);
            assert_eq!(meta.bit_depth, Some(bits));
            assert_eq!(meta.channels as usize, audio.spec().channels.count());
            break;
        }
    }

    // Synthetic 32-frame stereo 24-bit ramp encoded with ffmpeg ALAC at
    // 192 kHz, with the MP4 sample-entry rate replaced by the real-world
    // placeholder 1 Hz. The ALAC cookie and every audio sample are untouched.
    #[test]
    fn alac_placeholder_rate_preserves_signal_in_memory_and_streaming() {
        let bytes = include_bytes!("../../testdata/alac_24_192_placeholder.m4a");
        let meta = super::extract_audio_metadata_full(bytes).unwrap();
        assert_eq!(
            (meta.sample_rate, meta.bit_depth, meta.channels),
            (192000, Some(24), 2)
        );
        let expected: Vec<f32> = [-8388608, -4194304, -1, 0, 1, 4194304, 8388607, 1234567]
            .into_iter()
            .cycle()
            .take(64)
            .map(|v| v as f32 / 8388608.0)
            .collect();
        let memory = super::streaming_source::InMemorySource::new(bytes.to_vec()).unwrap();
        assert_eq!(memory.sample_rate(), 192000);
        assert_eq!(memory.collect::<Vec<_>>(), expected);
        let (buffer, writer) =
            BufferedMediaSource::new(StreamingConfig::default(), Some(bytes.len() as u64));
        writer.push_chunk(bytes).unwrap();
        writer.complete().unwrap();
        let streaming =
            super::streaming_source::IncrementalStreamingSource::new(std::sync::Arc::new(buffer))
                .unwrap();
        assert_eq!(streaming.get_sample_rate(), 192000);
        assert_eq!(streaming.get_channels(), 2);
        assert_eq!(streaming.collect::<Vec<_>>(), expected);
    }

    #[test]
    fn alac_metadata_rejects_invalid_config_and_keeps_other_codecs_unchanged() {
        use symphonia::core::codecs::{CodecParameters, CODEC_TYPE_ALAC, CODEC_TYPE_FLAC};
        let mut params = CodecParameters::new();
        params
            .for_codec(CODEC_TYPE_FLAC)
            .with_sample_rate(44100)
            .with_bits_per_sample(16);
        let meta = super::audio_metadata_from_codec_params(&params).unwrap();
        assert_eq!((meta.sample_rate, meta.bit_depth), (44100, Some(16)));
        params.for_codec(CODEC_TYPE_ALAC);
        assert!(super::audio_metadata_from_codec_params(&params).is_err());
        for len in [0, 23, 25, 47, 49] {
            params.with_extra_data(vec![0; len].into_boxed_slice());
            assert!(super::audio_metadata_from_codec_params(&params).is_err());
        }
        for rate in [44100_u32, 48000, 96000, 192000] {
            for bits in [16_u8, 24] {
                let mut cookie = vec![0; 24];
                cookie[5] = bits;
                cookie[9] = 2;
                cookie[20..24].copy_from_slice(&rate.to_be_bytes());
                params.with_extra_data(cookie.clone().into_boxed_slice());
                let meta = super::audio_metadata_from_codec_params(&params).unwrap();
                assert_eq!(
                    (meta.sample_rate, meta.bit_depth),
                    (rate, Some(bits as u32))
                );
                for index in [5, 9] {
                    let mut bad = cookie.clone();
                    bad[index] = 0;
                    params.with_extra_data(bad.into_boxed_slice());
                    assert!(super::audio_metadata_from_codec_params(&params).is_err());
                }
            }
        }
    }

    /// The streaming sources turn these into `NonZero` with an `unwrap` on the
    /// audio thread; a demuxer reporting `Some(0)` or an empty channel mask
    /// must fail here instead.
    #[test]
    fn codec_params_with_a_zero_rate_or_no_channels_are_rejected_before_nonzero() {
        use symphonia::core::audio::Channels;
        use symphonia::core::codecs::{CodecParameters, CODEC_TYPE_FLAC};
        let mut params = CodecParameters::new();
        params.for_codec(CODEC_TYPE_FLAC).with_sample_rate(0);
        assert!(super::audio_metadata_from_codec_params(&params).is_err());
        params.with_sample_rate(44100).with_channels(Channels::empty());
        assert!(super::audio_metadata_from_codec_params(&params).is_err());
        params.with_channels(Channels::FRONT_LEFT | Channels::FRONT_RIGHT);
        let meta = super::audio_metadata_from_codec_params(&params).unwrap();
        assert_eq!((meta.sample_rate, meta.channels), (44100, 2));
    }

    struct FakeDsdSource {
        words: std::vec::IntoIter<i32>,
        error: Option<String>,
    }

    /// #734: a CBR MP3 without a Xing/Info/VBRI frame (2 s, 32 kbps mono,
    /// `lame -t`). Symphonia can only estimate its duration from the byte
    /// length, which `Decoder::new` never passes along.
    #[test]
    fn local_decoder_derives_duration_without_xing_header() {
        let bytes = include_bytes!("../../testdata/cbr_no_xing.mp3");
        let source = super::decode_with_fallback(bytes).expect("fixture decodes");
        let duration = source
            .total_duration()
            .expect("duration estimated from the byte length");
        assert!(
            (1..=3).contains(&duration.as_secs()),
            "expected ~2 s, got {duration:?}"
        );
    }

    /// #734: an unknown (0) duration must not pin the playback clock to 0.
    #[test]
    fn unknown_duration_does_not_pin_position_to_zero() {
        let state = SharedState::new();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        state.is_playing.store(true, Ordering::SeqCst);
        state.position_at_start.store(0, Ordering::SeqCst);
        state
            .playback_start_millis
            .store(now_ms.saturating_sub(3_000), Ordering::SeqCst);

        state.duration.store(0, Ordering::SeqCst);
        assert!(state.current_position() >= 2, "unclamped clock advances");
        assert!(state.current_position_ms() >= 2_000);

        state.duration.store(1, Ordering::SeqCst);
        assert_eq!(state.current_position(), 1, "known duration still clamps");
        assert_eq!(state.current_position_ms(), 1_000);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inactive_direct_hardware_volume_reports_unity() {
        assert_eq!(reported_volume_after_command(0.25, true, false), 1.0);
        assert_eq!(reported_volume_after_command(0.25, true, true), 0.25);
        assert_eq!(reported_volume_after_command(0.25, false, false), 0.25);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unavailable_hardware_volume_session_cannot_be_revived_by_stale_event() {
        let state = SharedState::new();
        let callback = hardware_volume_event_callback(&state, true);
        let changed = |volume| {
            HardwareVolumeEvent::Changed(HardwareVolumeSnapshot {
                control: AlsaMixerControlId {
                    name: "D50 III".to_string(),
                    index: 0,
                },
                channels: Vec::new(),
                volume,
                muted: false,
            })
        };

        callback(changed(0.42));
        assert!(state.hardware_volume_active());
        assert_eq!(state.volume(), 0.42);

        callback(HardwareVolumeEvent::Unavailable(
            "device disappeared".to_string(),
        ));
        assert!(!state.hardware_volume_active());
        assert_eq!(state.volume(), 1.0);

        callback(changed(0.75));
        assert!(!state.hardware_volume_active());
        assert_eq!(state.volume(), 1.0);
    }

    impl Iterator for FakeDsdSource {
        type Item = i32;
        fn next(&mut self) -> Option<i32> {
            self.words.next()
        }
    }

    impl qbz_dsd::DsdWordSource for FakeDsdSource {
        fn io_error(&self) -> Option<&str> {
            self.error.as_deref()
        }
    }

    #[test]
    fn wedge_recovery_rebuilds_once_then_backs_off() {
        // #660: a wedged CPAL stream (POLLERR loop) is rebuilt at once; a
        // second wedge inside the backoff window means the device itself is
        // gone or broken, so the thread releases it and stops instead of
        // rebuilding in a loop.
        let t0 = std::time::Instant::now();
        assert_eq!(wedge_recovery(None, t0), WedgeRecovery::Rebuild);
        assert_eq!(
            wedge_recovery(Some(t0), t0 + std::time::Duration::from_secs(3)),
            WedgeRecovery::Stop
        );
        assert_eq!(
            wedge_recovery(Some(t0), t0 + WEDGE_REBUILD_BACKOFF),
            WedgeRecovery::Rebuild
        );
    }

    #[test]
    fn dsd_error_report_surfaces_io_error_at_stream_end() {
        let state = SharedState::new();
        let source = FakeDsdSource {
            words: vec![1, 2].into_iter(),
            error: Some("read failed".to_string()),
        };
        let mut wrapped = DsdErrorReport::new(source, state.clone());
        assert_eq!(wrapped.next(), Some(1));
        assert!(!state.has_stream_error());
        assert_eq!(wrapped.next(), Some(2));
        assert_eq!(wrapped.next(), None);
        assert!(state.has_stream_error());
        let msg = state.take_stream_error_message().unwrap();
        assert!(msg.contains("read failed"));
        // Repeated polls after exhaustion must not re-record the message.
        assert_eq!(wrapped.next(), None);
        assert_eq!(state.take_stream_error_message(), None);
    }

    #[test]
    fn dsd_error_report_clean_eof_is_not_an_error() {
        let state = SharedState::new();
        let source = FakeDsdSource {
            words: vec![1].into_iter(),
            error: None,
        };
        let mut wrapped = DsdErrorReport::new(source, state.clone());
        assert_eq!(wrapped.next(), Some(1));
        assert_eq!(wrapped.next(), None);
        assert!(!state.has_stream_error());
        assert_eq!(state.take_stream_error_message(), None);
    }

    #[test]
    fn engine_empty_edges_are_monotonic_and_keep_the_track_identity() {
        let state = SharedState::new();
        state.record_engine_empty(41);
        assert_eq!(state.engine_empty_generation.load(Ordering::SeqCst), 1);
        assert_eq!(state.engine_empty_track_id.load(Ordering::SeqCst), 41);

        state.record_engine_empty(42);
        assert_eq!(state.engine_empty_generation.load(Ordering::SeqCst), 2);
        assert_eq!(state.engine_empty_track_id.load(Ordering::SeqCst), 42);
    }

    #[test]
    fn superseded_feeder_cannot_publish_a_source_failure() {
        let state = SharedState::new();
        let old_generation = state.current_play_generation();
        state.begin_play();
        state.record_source_failure(41, old_generation);

        assert_eq!(state.source_failure_generation.load(Ordering::SeqCst), 0);
        assert_eq!(state.source_failure_track_id.load(Ordering::SeqCst), 0);

        let current_generation = state.current_play_generation();
        state.record_source_failure(42, current_generation);
        assert_eq!(state.source_failure_generation.load(Ordering::SeqCst), 1);
        assert_eq!(state.source_failure_track_id.load(Ordering::SeqCst), 42);
    }

    #[test]
    fn buffer_state_is_scoped_to_play_generation_and_track() {
        let state = SharedState::new();
        let first_generation = state.begin_play();
        state.begin_buffering(41, first_generation);
        let first_reporter = PlaybackBufferReporter::new(state.clone(), 41, first_generation);

        assert_eq!(
            state.playback_buffer_snapshot(),
            (PlaybackBufferState::InitialBuffering, 41)
        );
        first_reporter.report(PlaybackBufferState::Ready);
        assert_eq!(
            state.playback_buffer_snapshot(),
            (PlaybackBufferState::Ready, 41)
        );

        let second_generation = state.begin_play();
        state.begin_buffering(42, second_generation);
        first_reporter.report(PlaybackBufferState::Underrun);

        assert_eq!(
            state.playback_buffer_snapshot(),
            (PlaybackBufferState::InitialBuffering, 42),
            "a late transition from the old decoder must not overwrite the new play"
        );
    }

    #[test]
    fn current_source_failure_marks_buffer_error() {
        let state = SharedState::new();
        let generation = state.begin_play();
        state.begin_buffering(42, generation);

        state.record_source_failure(42, generation);

        assert_eq!(
            state.playback_buffer_snapshot(),
            (PlaybackBufferState::Error, 42)
        );
    }

    #[test]
    fn superseding_play_cancels_current_and_gapless_stream_feeders() {
        let state = SharedState::new();
        let (_, current_writer) =
            BufferedMediaSource::new(StreamingConfig::fast_start(), Some(1024));
        let (_, next_writer) =
            BufferedMediaSource::new(StreamingConfig::fast_start(), Some(1024));
        let (current_tx, current_rx) = tokio::sync::watch::channel(false);
        let (next_tx, next_rx) = tokio::sync::watch::channel(false);
        let generation = state.current_play_generation();
        state.register_stream_feeder(1, current_tx, current_writer, generation);
        assert!(state.register_gapless_stream_feeder(
            2,
            next_tx,
            next_writer,
            generation
        ));

        state.cancel_stream_feeder();

        assert!(*current_rx.borrow());
        assert!(*next_rx.borrow());
    }

    #[test]
    fn gapless_handoff_promotes_the_successor_feeder() {
        let state = SharedState::new();
        let (_, writer) = BufferedMediaSource::new(StreamingConfig::fast_start(), Some(1024));
        let (cancel_tx, _cancel_rx) = tokio::sync::watch::channel(false);
        let generation = state.current_play_generation();
        assert!(state.register_gapless_stream_feeder(
            42,
            cancel_tx,
            writer,
            generation
        ));

        state.promote_gapless_stream_feeder(42);

        assert_eq!(
            state
                .stream_feeder
                .lock()
                .unwrap()
                .as_ref()
                .map(|slot| slot.track_id),
            Some(42)
        );
        assert!(state.gapless_stream_feeder.lock().unwrap().is_none());
    }

    #[test]
    fn no_stream_always_needs_new() {
        // Without an existing stream there is nothing to reuse — every
        // other flag is irrelevant.
        assert!(compute_needs_new_stream(
            false, false, false, false, false, false
        ));
    }

    #[test]
    fn unchanged_format_on_default_backend_reuses_stream() {
        // Default rodio backend resamples internally, so an unchanged
        // decoded format on an existing stream needs no rebuild.
        assert!(!compute_needs_new_stream(
            true, false, false, false, false, false
        ));
    }

    #[test]
    fn format_change_on_default_backend_rebuilds_for_native_rate() {
        // #449 regression guard: a decoded sample-rate/channel change must
        // rebuild the output stream on EVERY backend, not just the bit-perfect
        // ones, so the device follows the track's native rate. 1.2.10 got this
        // "for free" because Stop dropped the stream; once that drop was
        // deferred to avoid a track-change click (e93fcaec), reusing the stream
        // on the default/PipeWire backend left the node locked to the first
        // track's rate for every subsequent track. Same-rate tracks still
        // reuse (format_changed == false), preserving the click fix.
        assert!(compute_needs_new_stream(
            true, true, false, false, false, false
        ));
    }

    #[test]
    fn format_change_with_dac_passthrough_rebuilds() {
        assert!(compute_needs_new_stream(
            true, true, true, false, false, false
        ));
    }

    #[test]
    fn format_change_with_alsa_direct_rebuilds() {
        assert!(compute_needs_new_stream(
            true, true, false, true, false, false
        ));
    }

    #[test]
    fn format_change_with_coreaudio_exclusive_rebuilds() {
        assert!(compute_needs_new_stream(
            true, true, false, false, true, false
        ));
    }

    #[test]
    fn bit_perfect_backends_without_format_change_reuse_stream() {
        // Bit-perfect flags only force a rebuild *together with* a format
        // change. On their own they should not.
        assert!(!compute_needs_new_stream(
            true, false, true, false, false, false
        ));
        assert!(!compute_needs_new_stream(
            true, false, false, true, false, false
        ));
        assert!(!compute_needs_new_stream(
            true, false, false, false, true, false
        ));
    }

    #[test]
    fn coreaudio_shared_rate_mismatch_rebuilds_regardless_of_format_change() {
        // The CoreAudio shared-mode rate-drift case has nothing to do with
        // track format; it must rebuild whenever detected.
        assert!(compute_needs_new_stream(
            true, false, false, false, false, true
        ));
    }

    #[test]
    fn current_position_ms_is_a_pure_anchor_derivation() {
        use std::sync::atomic::Ordering;

        let state = super::SharedState::new();
        state.duration.store(300, Ordering::SeqCst);

        // Paused: coarse stored position scaled to ms (current_position parity).
        state.position.store(12, Ordering::SeqCst);
        assert_eq!(state.current_position_ms(), 12_000);

        // Playing without an anchor yet: same coarse fallback.
        state.is_playing.store(true, Ordering::SeqCst);
        assert_eq!(state.current_position_ms(), 12_000);

        // Playing with anchors: position_at_start*1000 + wall-clock elapsed ms.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        state
            .playback_start_millis
            .store(now_ms - 1_500, Ordering::SeqCst);
        state.position_at_start.store(10, Ordering::SeqCst);
        let pos = state.current_position_ms();
        assert!(
            (11_450..=11_900).contains(&pos),
            "expected ~11500ms, got {pos}"
        );

        // Clamped to duration*1000 (same rule current_position applies).
        state
            .playback_start_millis
            .store(now_ms - 10_000, Ordering::SeqCst);
        state.position_at_start.store(299, Ordering::SeqCst);
        assert_eq!(state.current_position_ms(), 300_000);
    }

    #[test]
    fn external_content_type_prefers_server_mime() {
        assert_eq!(external_content_type("audio/flac", 7), "audio/flac");
        assert_eq!(external_content_type("audio/mpeg", 5), "audio/mpeg");
        // Whitespace-only is treated as empty.
        assert_eq!(external_content_type("  ", 6), "audio/flac");
    }

    #[test]
    fn external_content_type_falls_back_by_format_id() {
        // Empty MIME (Qobuz can return "") -> derive from format id.
        assert_eq!(external_content_type("", 5), "audio/mpeg"); // Mp3
        assert_eq!(external_content_type("", 6), "audio/flac"); // Lossless
        assert_eq!(external_content_type("", 7), "audio/flac"); // HiRes
        assert_eq!(external_content_type("", 27), "audio/flac"); // UltraHiRes
        assert_eq!(external_content_type("", 999), "audio/flac"); // unknown -> flac
    }

    #[test]
    fn stream_quality_normalizes_units() {
        use qbz_models::StreamQualityInfo;
        // kHz input stays kHz.
        let khz = StreamQualityInfo::from_raw(7, Some(96.0), Some(24));
        assert_eq!(khz.sampling_rate_khz, Some(96.0));
        // Hz input is converted to kHz.
        let hz = StreamQualityInfo::from_raw(27, Some(192000.0), Some(24));
        assert_eq!(hz.sampling_rate_khz, Some(192.0));
        // Zero / unknown -> None.
        let zero = StreamQualityInfo::from_raw(6, Some(0.0), Some(16));
        assert_eq!(zero.sampling_rate_khz, None);
        // Tier label from format id.
        assert_eq!(khz.tier_label(), "FLAC 24-bit/≤96kHz");
        assert_eq!(hz.tier_label(), "FLAC 24-bit/>96kHz");
    }
}
