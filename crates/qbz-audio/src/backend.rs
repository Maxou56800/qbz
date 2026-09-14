//! Audio backend abstraction
//!
//! Provides a unified interface for different audio backends (PipeWire, ALSA, PulseAudio)
//! allowing users to choose their preferred audio stack.

use rodio::MixerDeviceSink;
// CPAL traits + DeviceSinkBuilder are used by the cross-platform CpalDefaultBackend
// ("System") on every OS, including Linux where it opens the ALSA "default" PCM.
use rodio::{
    cpal::traits::{DeviceTrait, HostTrait},
    DeviceSinkBuilder,
};
use serde::{Deserialize, Serialize};

/// Supported audio backends
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioBackendType {
    /// PipeWire backend (modern, recommended)
    /// - Supports device selection without changing system default
    /// - Uses PULSE_SINK environment variable
    /// - Compatible with PulseAudio apps
    PipeWire,

    /// ALSA backend (direct hardware access)
    /// - True exclusive mode (blocks device for other apps)
    /// - Bit-perfect guaranteed
    /// - Lowest latency
    /// - Requires manual device selection (hw:X,Y)
    Alsa,

    /// PulseAudio backend (legacy compatibility)
    /// - Similar to PipeWire but older
    /// - Fallback for systems without PipeWire
    Pulse,

    /// JACK backend (#263 Tier 3 — pro-audio routing). Linux-only in practice.
    /// - QBZ appears as a first-class JACK client with stable ports
    ///   (`qbz:out_FL` / `qbz:out_FR`), patchable in qjackctl/qpwgraph/Reaper
    /// - Routing survives track changes (the client + ports live once)
    /// - NOT bit-perfect: the JACK graph runs at ONE fixed rate (audio is
    ///   resampled) — an opt-in routing-freedom mode; never touches the
    ///   bit-perfect ALSA-exclusive / DAC-passthrough paths.
    Jack,

    /// WASAPI EXCLUSIVE (Windows) - the bit-perfect path.
    /// - Takes the endpoint exclusively; other apps go silent
    /// - No resampling: the DAC is commanded to the track's own rate
    /// - Selecting a device is required, by ENDPOINT ID (Windows gives several
    ///   endpoints the same friendly name)
    /// The trait's `create_output_stream` for this type is the SHARED CPAL one:
    /// the exclusive stream bypasses the trait entirely, exactly as ALSA Direct
    /// and JACK do.
    WasapiExclusive,

    /// System default backend (non-Linux platforms)
    /// - Uses CPAL default host (CoreAudio on macOS, WASAPI on Windows)
    /// - Automatic device selection via OS audio system
    SystemDefault,
}

impl Default for AudioBackendType {
    fn default() -> Self {
        // "System" everywhere: the OOTB default plays through the OS default
        // output, shared with other apps (no bit-perfect, no `pactl`). Audiophile
        // users opt into PipeWire / ALSA explicitly. This was PipeWire on Linux,
        // which hard-required `pactl` and froze OOTB playback without it (#470).
        AudioBackendType::SystemDefault
    }
}

/// ALSA plugin type (only relevant for ALSA backend)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlsaPlugin {
    /// Direct hardware access (hw)
    /// - Bit-perfect, exclusive
    /// - No automatic format conversion
    /// - Blocks device for other apps
    Hw,

    /// Plug hardware access (plughw)
    /// - Automatic format conversion
    /// - Resampling if needed
    /// - Still relatively direct
    PlugHw,

    /// PCM device (default)
    /// - Generic ALSA device
    /// - Most compatible
    Pcm,
}

impl Default for AlsaPlugin {
    fn default() -> Self {
        // Hw is the audiophile choice
        AlsaPlugin::Hw
    }
}

/// Audio device information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioDevice {
    /// Internal device identifier (e.g., "hw:4,0" for ALSA, sink name for PipeWire)
    pub id: String,

    /// User-friendly display name
    pub name: String,

    /// Detailed description (optional)
    pub description: Option<String>,

    /// Whether this is the system default device
    pub is_default: bool,

    /// Maximum supported sample rate (if known)
    pub max_sample_rate: Option<u32>,

    /// Supported sample rates (common audio rates that the device supports)
    /// Contains values like 44100, 48000, 88200, 96000, 176400, 192000, etc.
    pub supported_sample_rates: Option<Vec<u32>>,

    /// Device bus type (for PipeWire): "usb", "pci", "bluetooth", or None
    pub device_bus: Option<String>,

    /// Whether this is a hardware device (has HARDWARE flag in PipeWire)
    pub is_hardware: bool,
}

/// Audio backend configuration
#[derive(Debug, Clone)]
pub struct BackendConfig {
    /// Backend type
    pub backend_type: AudioBackendType,

    /// Device ID (backend-specific)
    pub device_id: Option<String>,

    /// ALSA plugin (only used if backend_type == Alsa)
    pub alsa_plugin: Option<AlsaPlugin>,

    /// Sample rate (for stream creation)
    pub sample_rate: u32,

    /// Channels
    pub channels: u16,

    /// Exclusive mode flag
    pub exclusive_mode: bool,

    /// When true, force PipeWire clock.force-quantum for bit-perfect playback
    pub pw_force_bitperfect: bool,

    /// When true, skip `pactl set-default-sink` on stream creation.
    /// Preserves external routing (JACK, qjackctl, Reaper).
    pub skip_sink_switch: bool,
}

/// Result type for backend operations
pub type BackendResult<T> = Result<T, String>;

/// ALSA Direct stream error classification
/// Used to determine if fallback to plughw is appropriate
#[derive(Debug, Clone)]
pub enum AlsaDirectError {
    /// PCM format not supported by hardware (can fallback to plughw)
    UnsupportedFormat(String),
    /// Device is busy/in use by another application
    DeviceBusy(String),
    /// Permission denied to access device
    PermissionDenied(String),
    /// Invalid parameters (channels, sample rate)
    InvalidParams(String),
    /// Device not found
    DeviceNotFound(String),
    /// Generic/unknown error
    Other(String),
}

impl std::fmt::Display for AlsaDirectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlsaDirectError::UnsupportedFormat(msg) => write!(f, "Unsupported format: {}", msg),
            AlsaDirectError::DeviceBusy(msg) => write!(f, "Device busy: {}", msg),
            AlsaDirectError::PermissionDenied(msg) => write!(f, "Permission denied: {}", msg),
            AlsaDirectError::InvalidParams(msg) => write!(f, "Invalid parameters: {}", msg),
            AlsaDirectError::DeviceNotFound(msg) => write!(f, "Device not found: {}", msg),
            AlsaDirectError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl AlsaDirectError {
    /// Check if this error allows fallback to plughw
    pub fn allows_plughw_fallback(&self) -> bool {
        matches!(self, AlsaDirectError::UnsupportedFormat(_))
    }

    /// Create from raw ALSA error message
    pub fn from_alsa_error(msg: &str) -> Self {
        let msg_lower = msg.to_lowercase();

        if msg_lower.contains("no supported audio format")
            || msg_lower.contains("format")
            || msg_lower.contains("s24_3le")
            || msg_lower.contains("s24le")
            || msg_lower.contains("sample format")
        {
            AlsaDirectError::UnsupportedFormat(msg.to_string())
        } else if msg_lower.contains("busy")
            || msg_lower.contains("resource temporarily unavailable")
            || msg_lower.contains("device or resource busy")
        {
            AlsaDirectError::DeviceBusy(msg.to_string())
        } else if msg_lower.contains("permission")
            || msg_lower.contains("access denied")
            || msg_lower.contains("operation not permitted")
        {
            AlsaDirectError::PermissionDenied(msg.to_string())
        } else if msg_lower.contains("not found")
            || msg_lower.contains("no such")
            || msg_lower.contains("doesn't exist")
        {
            AlsaDirectError::DeviceNotFound(msg.to_string())
        } else if msg_lower.contains("invalid")
            || msg_lower.contains("channels")
            || msg_lower.contains("rate")
        {
            AlsaDirectError::InvalidParams(msg.to_string())
        } else {
            AlsaDirectError::Other(msg.to_string())
        }
    }
}

/// Runtime mode for bit-perfect status tracking
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BitPerfectMode {
    /// Direct hardware access (hw:), guaranteed bit-perfect
    DirectHardware,
    /// Plugin hardware fallback (plughw:), bit-perfect with format conversion only
    PluginFallback,
    /// Not using bit-perfect path (pcm, pipewire, pulse)
    Disabled,
}

/// Audio backend trait
///
/// All audio backends must implement this trait to provide
/// a consistent interface for device enumeration and stream creation.
pub trait AudioBackend: Send + Sync {
    /// Get the backend type
    fn backend_type(&self) -> AudioBackendType;

    /// Enumerate available audio devices for this backend
    fn enumerate_devices(&self) -> BackendResult<Vec<AudioDevice>>;

    /// Create an output stream for the given configuration
    fn create_output_stream(&self, config: &BackendConfig) -> BackendResult<MixerDeviceSink>;

    /// Create an output stream and optionally return a platform exclusive-mode guard.
    /// Most backends do not need a guard; macOS CoreAudio uses this to keep Hog Mode
    /// owned for the lifetime of the stream.
    fn create_output_stream_with_exclusive_guard(
        &self,
        config: &BackendConfig,
    ) -> BackendResult<(
        MixerDeviceSink,
        Option<crate::coreaudio_direct::CoreAudioExclusiveGuard>,
    )> {
        self.create_output_stream(config).map(|sink| (sink, None))
    }

    /// Check if this backend is available on the current system
    fn is_available(&self) -> bool;

    /// Get a description of this backend for UI display
    fn description(&self) -> &'static str;

    /// Downcast to concrete type (for ALSA Direct stream creation)
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Backend manager - factory for creating backends
pub struct BackendManager;

impl BackendManager {
    /// Get all available backends on this system
    pub fn available_backends() -> Vec<AudioBackendType> {
        let mut backends = Vec::new();

        #[cfg(target_os = "linux")]
        {
            // System default (always available): shared OS default output via the
            // ALSA "default" PCM. The app-like OOTB choice; listed first.
            backends.push(AudioBackendType::SystemDefault);

            // PipeWire (check if running)
            if Self::is_pipewire_available() {
                backends.push(AudioBackendType::PipeWire);
            }

            // ALSA (always available on Linux)
            backends.push(AudioBackendType::Alsa);

            // PulseAudio (check if running)
            if Self::is_pulse_available() {
                backends.push(AudioBackendType::Pulse);
            }

            // JACK (#263 Tier 3): offered now that the player wiring is in place
            // (StreamType::Jack + dispatch + PlaybackEngine::Jack feeder/resampler).
            // The binary links libjack, so reaching here means it is present;
            // opening the client fails gracefully if no JACK server is reachable.
            backends.push(AudioBackendType::Jack);
        }

        #[cfg(not(target_os = "linux"))]
        {
            backends.push(AudioBackendType::SystemDefault);
        }

        #[cfg(windows)]
        {
            // Offered unconditionally: whether a given DEVICE accepts exclusive
            // mode is answered per device when the stream opens, not here, and
            // hiding the backend would hide the answer too.
            backends.push(AudioBackendType::WasapiExclusive);
        }

        backends
    }

    /// Create a backend instance
    pub fn create_backend(backend_type: AudioBackendType) -> BackendResult<Box<dyn AudioBackend>> {
        // Install the custom ALSA error handler once per process, before any
        // CPAL/ALSA enumeration fires. Idempotent via std::sync::Once.
        #[cfg(target_os = "linux")]
        crate::alsa_error_handler::install_once();

        match backend_type {
            AudioBackendType::PipeWire => {
                #[cfg(target_os = "linux")]
                {
                    let backend = crate::pipewire_backend::PipeWireBackend::new()?;
                    Ok(Box::new(backend))
                }
                #[cfg(not(target_os = "linux"))]
                {
                    log::info!(
                        "PipeWire not available on this platform, using system default audio"
                    );
                    Ok(Box::new(CpalDefaultBackend::new()?))
                }
            }
            AudioBackendType::WasapiExclusive => {
                // The SHARED stream, deliberately. `create_stream_for_backend`
                // tries the exclusive stream first and only reaches here when
                // that is impossible - no device chosen, or the endpoint
                // refusing every format - so this is the graceful fallback
                // rather than the intended path.
                Ok(Box::new(CpalDefaultBackend::new()?))
            }
            AudioBackendType::SystemDefault => {
                // "System": play through the OS default output via CPAL's default
                // host — CoreAudio/WASAPI off-Linux, the ALSA "default" PCM on Linux
                // (routes to PipeWire/Pulse/dmix for shared mixing). Opens at the
                // device's negotiated rate (rodio resamples); shared, no exclusivity,
                // no `pactl`. Available on every platform.
                Ok(Box::new(CpalDefaultBackend::new()?))
            }
            AudioBackendType::Alsa => {
                #[cfg(target_os = "linux")]
                {
                    let backend = crate::alsa_backend::AlsaBackend::new()?;
                    Ok(Box::new(backend))
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Err("ALSA backend only available on Linux".to_string())
                }
            }
            AudioBackendType::Pulse => {
                #[cfg(target_os = "linux")]
                {
                    let backend = crate::pulse_backend::PulseBackend::new()?;
                    Ok(Box::new(backend))
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Err("PulseAudio backend only available on Linux".to_string())
                }
            }
            AudioBackendType::Jack => {
                // JACK streams are created directly in the player dispatch
                // (qbz-player), NOT via the MixerDeviceSink AudioBackend trait.
                // This arm exists only so the factory stays exhaustive; the
                // returned backend is never used to open a JACK stream.
                #[cfg(target_os = "linux")]
                {
                    Ok(Box::new(CpalDefaultBackend::new()?))
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Err("JACK backend only available on Linux".to_string())
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn is_pipewire_available() -> bool {
        // Detect PipeWire via its runtime socket ($XDG_RUNTIME_DIR/pipewire-0),
        // which exists whenever PipeWire is running. Unlike `pactl`, this does
        // NOT require pulseaudio-utils to be installed — PipeWire-only systems
        // frequently lack it, which used to hide the PipeWire backend entirely
        // (issue #466). pw-cli / pactl remain as fallbacks for unusual setups
        // (e.g. a non-default socket name, or Flatpak where the socket path
        // differs but the pulse shim is bridged).
        if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            if std::path::Path::new(&runtime_dir)
                .join("pipewire-0")
                .exists()
            {
                return true;
            }
        }
        if std::process::Command::new("pw-cli")
            .args(["info", "0"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return true;
        }
        std::process::Command::new("pactl")
            .arg("info")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[cfg(target_os = "linux")]
    fn is_pulse_available() -> bool {
        // Check if PulseAudio is running
        std::process::Command::new("pactl")
            .arg("info")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// CPAL default backend ("System"): plays through the OS default output via
/// CPAL's default host — CoreAudio (macOS), WASAPI (Windows), and the ALSA
/// "default" PCM (Linux, which routes to PipeWire/Pulse/dmix for shared mixing).
/// No platform-specific commands (no `pactl`); opens at the device's negotiated
/// rate with rodio resampling, so it mixes with other apps like any normal player.
pub struct CpalDefaultBackend {
    host: rodio::cpal::Host,
}

impl CpalDefaultBackend {
    pub fn new() -> BackendResult<Self> {
        Ok(Self {
            host: rodio::cpal::default_host(),
        })
    }
}

impl AudioBackend for CpalDefaultBackend {
    fn backend_type(&self) -> AudioBackendType {
        AudioBackendType::SystemDefault
    }

    fn enumerate_devices(&self) -> BackendResult<Vec<AudioDevice>> {
        let default_device = self
            .host
            .default_output_device()
            .ok_or_else(|| "No default output device found".to_string())?;

        let default_name = default_device
            .description()
            .map(|desc| desc.name().to_string())
            .unwrap_or_else(|_| "Default Output".to_string());

        // WINDOWS ONLY, additive. Endpoint FRIENDLY NAMES are not unique
        // there: measured on a real box, three active render endpoints were
        // all called "Altavoces" - the motherboard Realtek, a Bluetooth
        // handsfree and a Cambridge Audio USB DAC. Matching the default by
        // name would mark all three.
        #[cfg(windows)]
        let default_endpoint_id = default_device.id().ok().map(|i| i.1);

        // Idempotent, and this is the natural place: the first time anything
        // asks what devices exist is the first time it matters that the answer
        // can go stale.
        #[cfg(windows)]
        crate::wasapi_backend::start_hotplug_watch();

        let mut devices = Vec::new();
        for device in self
            .host
            .output_devices()
            .map_err(|e| format!("Failed to enumerate output devices: {}", e))?
        {
            let name = device
                .description()
                .map(|desc| desc.name().to_string())
                .unwrap_or_else(|_| "Unknown Device".to_string());

            // On macOS, probe device capabilities via CoreAudio
            #[cfg(target_os = "macos")]
            let (supported_rates, max_rate, bus_type, is_hw) = { Self::probe_macos_device(&name) };
            // The placeholder for every platform that has no probe. On
            // Windows `supported_rates`/`max_rate` are SHADOWED further down
            // by the exclusive-mode sweep, which is why they carry an
            // underscore prefix there and nowhere else.
            #[cfg(all(not(target_os = "macos"), not(windows)))]
            let (supported_rates, max_rate, bus_type, is_hw): (
                Option<Vec<u32>>,
                Option<u32>,
                Option<String>,
                bool,
            ) = (None, None, None, false);
            #[cfg(windows)]
            let (bus_type, is_hw): (Option<String>, bool) = (None, false);

            // WINDOWS ONLY, additive: id, display and description all come
            // from fields cpal already fills but this backend ignored.
            //
            // `name` is the endpoint ("Altavoces") and repeats across
            // devices; `driver()` is the ADAPTER ("Cambridge Audio USB Audio
            // 2.0") and is what tells them apart; `id()` is the stable WASAPI
            // endpoint GUID. Using the name as the id meant device_filter
            // folded every same-named endpoint into one entry, so a USB DAC
            // sharing the name "Altavoces" with the onboard card could not be
            // selected at all - and an unselectable DAC blocks bit-perfect.
            #[cfg(windows)]
            let (id, display, description, is_default) = {
                let driver = device
                    .description()
                    .ok()
                    .and_then(|d| d.driver().map(str::to_string))
                    .filter(|d| !d.is_empty() && d.as_str() != name.as_str());
                let endpoint_id = device.id().ok().map(|i| i.1);
                let display = match &driver {
                    Some(d) => format!("{name} ({d})"),
                    None => name.clone(),
                };
                let is_default = match (&endpoint_id, &default_endpoint_id) {
                    (Some(a), Some(b)) => a == b,
                    _ => name == default_name,
                };
                (
                    endpoint_id.unwrap_or_else(|| name.clone()),
                    display,
                    driver,
                    is_default,
                )
            };
            #[cfg(not(windows))]
            let (id, display, description, is_default) =
                (name.clone(), name.clone(), None, name == default_name);

            // WINDOWS: the rates the endpoint accepts in EXCLUSIVE mode, which
            // is the only list that describes what reaches the converter
            // untouched. Shadows the `(None, None, ...)` placeholder above.
            //
            // It cannot be inferred, which is the whole reason it is measured:
            // the owner's DAC accepts 44100/48000/88200/96000/192000 and
            // REFUSES 176400, so the 44.1 family stops at 88.2 while the 48
            // family reaches 192. Cached per endpoint, so this costs once.
            //
            // `supported_rates` answers `None` when the endpoint could not
            // be asked at all -- busy, gone, or COM refused -- and that must
            // not reach a caller as "supports nothing", which would narrow a
            // quality cap on the strength of a failed question. An endpoint
            // that answers and lists nothing is reported the same way for the
            // same reason: there is nothing to offer either way, and neither
            // is a fact worth caching upstream.
            #[cfg(windows)]
            let (supported_rates, max_rate) = {
                match crate::wasapi_backend::supported_rates(&id) {
                    Some(rates) if !rates.is_empty() => {
                        let top = rates.iter().copied().max();
                        (Some(rates), top)
                    }
                    _ => (None, None),
                }
            };

            devices.push(AudioDevice {
                id,
                name: display,
                description,
                is_default,
                max_sample_rate: max_rate,
                supported_sample_rates: supported_rates,
                device_bus: bus_type,
                is_hardware: is_hw,
            });
        }

        // CPAL's raw enumeration repeats one output across many ALSA PCM
        // plugins (all sharing a display name) and includes the `null` discard
        // sink. Collapse the duplicates and push `null` to the end so the
        // System/JACK picker shows one entry per real output. See device_filter.
        let devices = crate::device_filter::retain_real_outputs(
            devices,
            |d| d.id.as_str(),
            |d| d.name.as_str(),
        );

        Ok(devices)
    }

    fn create_output_stream(&self, config: &BackendConfig) -> BackendResult<MixerDeviceSink> {
        #[cfg(target_os = "macos")]
        let macos_exclusive_device_name = if config.exclusive_mode && config.device_id.is_none() {
            match crate::coreaudio_direct::resolve_output_device_name(None) {
                Ok(name) => {
                    log::info!(
                        "[CoreAudio] Resolved System Default to '{}' for exclusive stream",
                        name
                    );
                    Some(name)
                }
                Err(e) => {
                    log::warn!(
                        "[CoreAudio] Could not resolve System Default device name: {}",
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

        // On macOS, only exclusive mode takes ownership of the device rate.
        // Shared mode must leave the user's current CoreAudio device rate alone.
        #[cfg(target_os = "macos")]
        {
            if config.exclusive_mode {
                if let Some(ref device_id) = config.device_id {
                    Self::switch_sample_rate_if_needed(device_id, config.sample_rate);
                } else if let Some(ref device_name) = macos_exclusive_device_name {
                    Self::switch_sample_rate_if_needed(device_name, config.sample_rate);
                } else {
                    Self::switch_default_device_rate_if_needed(config.sample_rate);
                }
            }
        }

        #[cfg(target_os = "macos")]
        let effective_device_id = config
            .device_id
            .as_ref()
            .or(macos_exclusive_device_name.as_ref());
        #[cfg(not(target_os = "macos"))]
        let effective_device_id = config.device_id.as_ref();

        #[cfg(target_os = "macos")]
        if !config.exclusive_mode {
            return self.open_macos_shared_stream_with_retry(
                effective_device_id.map(|name| name.as_str()),
                MACOS_SHARED_OPEN_MAX_ATTEMPTS,
            );
        }

        let device = if let Some(device_id) = effective_device_id {
            self.host
                .output_devices()
                .map_err(|e| format!("Failed to enumerate devices: {}", e))?
                .find(|d| {
                    // WINDOWS ONLY, additive: enumerate_devices publishes the
                    // stable endpoint GUID as the id there, because friendly
                    // names repeat. The name comparison stays as the fallback
                    // so a setting saved before this change still resolves.
                    #[cfg(windows)]
                    if d.id().map(|i| i.1 == *device_id).unwrap_or(false) {
                        return true;
                    }
                    d.description()
                        .map(|desc| desc.name() == device_id.as_str())
                        .unwrap_or(false)
                })
                .ok_or_else(|| format!("Device '{}' not found", device_id))?
        } else {
            self.host
                .default_output_device()
                .ok_or_else(|| "No default output device found".to_string())?
        };

        let builder = DeviceSinkBuilder::from_device(device)
            .map_err(|e| format!("Failed to create device sink builder: {}", e))?;

        // MixerDeviceSink has zero internal buffering, so CPAL's buffer is the
        // ONLY buffer between the mixer and the hardware. With the bare CPAL/ALSA
        // default (no explicit size) the stream can underrun immediately on Linux:
        // the node links to the audio server but stays suspended and never feeds
        // audio (#470 — "System" shared output silent). Give it ~100ms, matching
        // the PipeWire/ALSA backends. We deliberately do NOT pin the sample rate
        // (no with_supported_config): the device keeps its negotiated rate and
        // rodio resamples, which is the whole point of shared "System" output.
        #[cfg(target_os = "linux")]
        let mixer_sink = builder
            .with_buffer_size(rodio::cpal::BufferSize::Fixed(
                (config.sample_rate / 10).clamp(1024, 19200),
            ))
            // #660: rodio's default callback eprintln!s every error of a
            // wedged PCM (~1.7 M/s); the shared callback rate-limits it and
            // latches the wedge for the audio thread to rebuild the stream.
            .with_error_callback(crate::stream_health::error_callback("System default"))
            .open_stream()
            .map_err(|e| format!("Failed to create output stream: {}", e))?;
        #[cfg(not(target_os = "linux"))]
        let mixer_sink = builder
            .with_error_callback(crate::stream_health::error_callback("System default"))
            .open_stream()
            .map_err(|e| format!("Failed to create output stream: {}", e))?;

        Ok(mixer_sink)
    }

    #[cfg(target_os = "macos")]
    fn create_output_stream_with_exclusive_guard(
        &self,
        config: &BackendConfig,
    ) -> BackendResult<(
        MixerDeviceSink,
        Option<crate::coreaudio_direct::CoreAudioExclusiveGuard>,
    )> {
        if !config.exclusive_mode {
            return self.create_output_stream(config).map(|sink| (sink, None));
        }

        // Resolve the target device ONCE before acquiring Hog Mode and pin its
        // name into the config we hand to `create_output_stream`. Without this,
        // the rate-switch and CPAL-stream paths inside `create_output_stream`
        // re-resolve the System Default by name — and macOS reassigns the
        // System Default the moment we hog the previous one (so other apps
        // still get audio). The result is Hog Mode held on device A while the
        // stream is created on device B, which on a multi-device machine
        // (e.g. an external DAC alongside built-in speakers) leaves the DAC
        // hogged-but-unused and audio routed to a device that may not even
        // support the requested sample rate.
        let device_id =
            crate::coreaudio_direct::resolve_output_device_id(config.device_id.as_deref())?;
        let device_name = crate::coreaudio_direct::get_device_name(device_id)?;
        let guard = crate::coreaudio_direct::CoreAudioExclusiveGuard::acquire(device_id)?;

        let mut effective_config = config.clone();
        effective_config.device_id = Some(device_name);

        self.create_output_stream(&effective_config)
            .map(|sink| (sink, Some(guard)))
    }

    fn is_available(&self) -> bool {
        self.host.default_output_device().is_some()
    }

    fn description(&self) -> &'static str {
        "System Audio - Default audio output"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(target_os = "macos")]
const MACOS_SHARED_OPEN_MAX_ATTEMPTS: usize = 2;
#[cfg(target_os = "macos")]
const MACOS_SHARED_OPEN_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

#[cfg(target_os = "macos")]
impl CpalDefaultBackend {
    /// Open a macOS shared-mode CPAL stream and verify it actually opened at
    /// CoreAudio's current nominal rate. CPAL caches device configs and can
    /// return a stale rate when the OS rate has just changed; passing the
    /// stale rate to CPAL produces a stream that runs at the wrong speed.
    ///
    /// Retries up to `max_attempts` times because the mismatch is racy — a
    /// fresh resolve + reopen usually lands on the correct rate.
    fn open_macos_shared_stream_with_retry(
        &self,
        effective_device_name: Option<&str>,
        max_attempts: usize,
    ) -> BackendResult<MixerDeviceSink> {
        debug_assert!(max_attempts >= 1, "max_attempts must be at least 1");

        let mut last_err: Option<String> = None;
        for attempt in 0..max_attempts {
            let device = if let Some(device_name) = effective_device_name {
                self.host
                    .output_devices()
                    .map_err(|e| format!("Failed to enumerate devices: {}", e))?
                    .find(|d| {
                        d.description()
                            .map(|desc| desc.name() == device_name)
                            .unwrap_or(false)
                    })
                    .ok_or_else(|| format!("Device '{}' not found", device_name))?
            } else {
                self.host
                    .default_output_device()
                    .ok_or_else(|| "No default output device found".to_string())?
            };

            let override_config =
                Self::shared_mode_nominal_stream_config(&device, effective_device_name);

            let builder = DeviceSinkBuilder::from_device(device)
                .map_err(|e| format!("Failed to create device sink builder: {}", e))?;

            let open_result = if let Some(ref cfg) = override_config {
                let buffer_size = rodio::cpal::BufferSize::Fixed((cfg.sample_rate() / 20).max(64));
                builder
                    .with_supported_config(cfg)
                    .with_buffer_size(buffer_size)
                    .with_error_callback(crate::stream_health::error_callback("System default"))
                    .open_stream()
            } else {
                builder
                    .with_error_callback(crate::stream_health::error_callback("System default"))
                    .open_stream()
            };

            let mixer_sink = match open_result {
                Ok(s) => s,
                Err(e) => {
                    last_err = Some(format!("Failed to create output stream: {}", e));
                    break;
                }
            };

            let nominal_rate = Self::current_macos_nominal_rate(effective_device_name);
            let output_rate = mixer_sink.config().sample_rate().get();
            match nominal_rate {
                Some(nominal) if nominal != output_rate => {
                    drop(mixer_sink);
                    if attempt + 1 < max_attempts {
                        log::warn!(
                            "[CoreAudio] System audio rate changed during stream open (stream {}Hz, device now {}Hz). Retrying open ({}/{})",
                            output_rate,
                            nominal,
                            attempt + 2,
                            max_attempts
                        );
                        std::thread::sleep(MACOS_SHARED_OPEN_RETRY_DELAY);
                    }
                    last_err = Some(format!(
                        "macOS audio device changed its sample rate during stream open (opened at {}Hz, device is now {}Hz). This usually self-corrects — try playing again.",
                        output_rate, nominal
                    ));
                    continue;
                }
                _ => return Ok(mixer_sink),
            }
        }

        Err(last_err.unwrap_or_else(|| {
            "Could not open the macOS audio output stream after multiple attempts. Try selecting the device again or restarting playback.".to_string()
        }))
    }

    fn current_macos_nominal_rate(effective_device_name: Option<&str>) -> Option<u32> {
        use crate::coreaudio_direct;

        let device_id = match effective_device_name {
            Some(name) => coreaudio_direct::find_device_by_name(name).ok().flatten(),
            None => coreaudio_direct::get_default_output_device().ok(),
        }?;

        coreaudio_direct::get_nominal_sample_rate(device_id).ok()
    }

    /// In macOS shared mode, CPAL's default config can briefly report a stale
    /// sample rate after CoreAudio changes the device's nominal rate. If we
    /// trust the stale rate, playback can run at the wrong speed until the
    /// stream is recreated. Prefer opening the CPAL stream at CoreAudio's
    /// current nominal rate when the two disagree.
    fn shared_mode_nominal_stream_config(
        device: &rodio::cpal::Device,
        effective_device_name: Option<&str>,
    ) -> Option<rodio::cpal::SupportedStreamConfig> {
        let nominal_rate = Self::current_macos_nominal_rate(effective_device_name)?;
        let default_config = device.default_output_config().ok()?;
        let default_rate = default_config.sample_rate();
        if nominal_rate == default_rate {
            return None;
        }

        let supported_configs: Vec<_> = device.supported_output_configs().ok()?.collect();
        let matching_config = supported_configs
            .iter()
            .find_map(|range| {
                if range.channels() == default_config.channels()
                    && range.sample_format() == default_config.sample_format()
                {
                    (*range).try_with_sample_rate(nominal_rate)
                } else {
                    None
                }
            })
            .or_else(|| {
                supported_configs
                    .iter()
                    .find_map(|range| (*range).try_with_sample_rate(nominal_rate))
            });

        let device_label = effective_device_name.unwrap_or("System Default");
        if matching_config.is_some() {
            log::warn!(
                "[CoreAudio] Shared-mode rate mismatch on '{}': CPAL default {}Hz vs CoreAudio nominal {}Hz. Opening stream at the nominal rate to avoid wrong-speed playback.",
                device_label,
                default_rate,
                nominal_rate
            );
        } else {
            log::warn!(
                "[CoreAudio] Shared-mode rate mismatch on '{}': CPAL default {}Hz vs CoreAudio nominal {}Hz, but no supported CPAL config matched the nominal rate.",
                device_label,
                default_rate,
                nominal_rate
            );
        }

        matching_config
    }

    /// Probe a macOS audio device for capabilities via CoreAudio APIs.
    /// Returns (supported_rates, max_rate, bus_type, is_hardware).
    fn probe_macos_device(
        device_name: &str,
    ) -> (Option<Vec<u32>>, Option<u32>, Option<String>, bool) {
        use crate::coreaudio_direct;

        let device_id = match coreaudio_direct::find_device_by_name(device_name) {
            Ok(Some(id)) => {
                log::info!("[CoreAudio] Found device '{}' with ID {}", device_name, id);
                id
            }
            Ok(None) => {
                log::debug!(
                    "[CoreAudio] Device '{}' not found via CoreAudio",
                    device_name
                );
                return (None, None, None, false);
            }
            Err(e) => {
                log::debug!("[CoreAudio] Error finding device '{}': {}", device_name, e);
                return (None, None, None, false);
            }
        };

        let supported_rates = coreaudio_direct::query_supported_sample_rates(device_id)
            .inspect(|rates| {
                log::info!(
                    "[CoreAudio] Device '{}' supported rates: {:?}",
                    device_name,
                    rates
                )
            })
            .inspect_err(|e| {
                log::warn!(
                    "[CoreAudio] Failed to query rates for '{}': {}",
                    device_name,
                    e
                )
            })
            .ok()
            .filter(|r| !r.is_empty());
        let max_rate = supported_rates
            .as_ref()
            .and_then(|rates| rates.iter().max().copied());
        let bus_type = coreaudio_direct::get_device_transport_type(device_id);
        let is_hardware = bus_type.as_deref().is_some_and(|t| {
            t == "usb" || t == "built-in" || t == "thunderbolt" || t == "firewire"
        });

        (supported_rates, max_rate, bus_type, is_hardware)
    }

    /// Switch device sample rate before stream creation (if device supports the target rate).
    fn switch_sample_rate_if_needed(device_name: &str, target_rate: u32) {
        use crate::coreaudio_direct;

        log::info!(
            "[CoreAudio] Rate switch requested: device='{}' target={}Hz",
            device_name,
            target_rate
        );

        let device_id = match coreaudio_direct::find_device_by_name(device_name) {
            Ok(Some(id)) => id,
            Ok(None) => {
                log::warn!(
                    "[CoreAudio] Cannot switch rate: device '{}' not found",
                    device_name
                );
                return;
            }
            Err(e) => {
                log::warn!("[CoreAudio] Cannot switch rate: {}", e);
                return;
            }
        };

        // Check if device supports the target rate
        if let Ok(rates) = coreaudio_direct::query_supported_sample_rates(device_id) {
            if !rates.contains(&target_rate) {
                log::debug!(
                    "[CoreAudio] Device '{}' does not support {}Hz, skipping rate switch",
                    device_name,
                    target_rate
                );
                return;
            }
        }

        if let Err(e) = coreaudio_direct::set_nominal_sample_rate(device_id, target_rate) {
            log::warn!("[CoreAudio] Failed to switch sample rate: {}", e);
        }
    }

    /// Switch the default output device's sample rate.
    fn switch_default_device_rate_if_needed(target_rate: u32) {
        use crate::coreaudio_direct;

        let device_id = match coreaudio_direct::get_default_output_device() {
            Ok(id) => id,
            Err(e) => {
                log::debug!(
                    "[CoreAudio] Could not get default device for rate switch: {}",
                    e
                );
                return;
            }
        };

        if let Ok(rates) = coreaudio_direct::query_supported_sample_rates(device_id) {
            if !rates.contains(&target_rate) {
                log::debug!(
                    "[CoreAudio] Default device does not support {}Hz, skipping rate switch",
                    target_rate
                );
                return;
            }
        }

        if let Err(e) = coreaudio_direct::set_nominal_sample_rate(device_id, target_rate) {
            log::warn!(
                "[CoreAudio] Failed to switch default device sample rate: {}",
                e
            );
        }
    }
}

/// The two-method surface a bit-perfect direct stream owes the player's writer
/// thread, plus the three it owes the engine around it.
///
/// It exists so ONE writer thread serves both `AlsaDirectStream` and
/// `WasapiDirectStream` instead of two copies drifting apart. The set is
/// exactly what `playback_engine.rs` already called on the ALSA stream - no
/// method was invented for the trait, and neither implementation gains or
/// loses behaviour by having it.
#[derive(Debug)]
pub struct DirectWriteError {
    pub frames_written: usize,
    pub canceled: bool,
    pub message: String,
}

impl From<String> for DirectWriteError {
    fn from(message: String) -> Self {
        Self { frames_written: 0, canceled: false, message }
    }
}

pub trait DirectSink: Send + Sync + 'static {
    /// Interleaved f32, blocking when the device's queue is full. That block
    /// IS the back-pressure that paces the writer thread.
    fn write_f32(&self, samples: &[f32]) -> Result<(), String>;
    /// Backends with interruptible I/O override this to report partial progress.
    fn write_f32_interruptible(&self, samples: &[f32], cancel: &std::sync::atomic::AtomicBool)
        -> Result<usize, DirectWriteError> {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(DirectWriteError { frames_written: 0, canceled: true, message: "write canceled".into() });
        }
        let channels = usize::from(self.channels());
        if channels == 0 || samples.len() % channels != 0 {
            return Err(DirectWriteError::from("PCM input must contain whole frames".to_string()));
        }
        self.write_f32(samples).map_err(DirectWriteError::from)?;
        Ok(samples.len() / channels)
    }
    /// Play out what is queued, then return. Bounded internally.
    fn drain(&self) -> Result<(), String>;
    fn drain_interruptible(&self, _cancel: &std::sync::atomic::AtomicBool) -> Result<(), String> {
        self.drain()
    }
    fn stop(&self) -> Result<(), String>;
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u16;

    /// Frames submitted to the sink but not yet heard, when the backend can
    /// measure them. The visualizer uses this only as latency compensation;
    /// an unsupported query keeps playback intact and falls back to the
    /// bounded direct-writer pacing.
    fn playback_delay_frames(&self) -> Result<u64, String> {
        Err("this direct sink cannot measure queued playback frames".to_string())
    }

    /// Log prefix for this sink. On the trait rather than a parameter so a
    /// call site cannot label an ALSA stream "WASAPI" or the reverse - which
    /// is exactly what a hard-coded label at four call sites invites.
    fn log_label(&self) -> &'static str;

    /// ALSA PCM identity for control-plane operations. Other direct sinks do
    /// not expose an ALSA route and keep the default `None`.
    fn alsa_device_id(&self) -> Option<&str> {
        None
    }

    /// Device-side volume, where the sink has one.
    ///
    /// The default REFUSES rather than silently succeeding: a sink without a
    /// hardware mixer must not pretend it moved the volume. Every call site is
    /// already gated on the engine's `hardware_volume` flag, which is false for
    /// any such sink - permanently so for WASAPI exclusive, where the phase B
    /// contract rules out ISimpleAudioVolume.
    fn set_hardware_volume(
        &self,
        _control: &crate::alsa_hardware_volume::AlsaMixerControlId,
        _volume: f32,
    ) -> Result<(), String> {
        Err("this sink has no hardware volume".to_string())
    }
}
