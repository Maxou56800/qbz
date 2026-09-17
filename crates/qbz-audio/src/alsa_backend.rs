//! ALSA audio backend (direct hardware access)
//!
//! Provides direct access to ALSA hardware devices for:
//! - True exclusive mode (blocks device for other apps)
//! - Bit-perfect playback (no resampling)
//! - Low-latency audio output
//!
//! Uses CPAL's ALSA host with specific device selection.
//! Device enumeration reads directly from /proc/asound (no alsa-utils dependency).

use super::backend::{
    AlsaPlugin, AudioBackend, AudioBackendType, AudioDevice, BackendConfig, BackendResult,
};
use rodio::{
    cpal::{
        traits::{DeviceTrait, HostTrait},
        BufferSize, SampleFormat, StreamConfig, SupportedBufferSize, SupportedStreamConfig,
    },
    DeviceSinkBuilder, MixerDeviceSink,
};
use std::collections::HashMap;
use std::fs;
use std::sync::Mutex;

/// Keep scheduling headroom in milliseconds as hi-res rates increase. CPAL
/// uses this as one period and allocates two periods on ALSA. Respect the
/// driver's actual bounds, not the synthetic SupportedStreamConfig below.
fn playback_period_frames(rate: u32, exclusive: bool, bounds: &SupportedBufferSize) -> u32 {
    let target = (rate / if exclusive { 20 } else { 10 }).max(1);
    match *bounds {
        SupportedBufferSize::Range { min, max } if min > 0 && max >= min => target.clamp(min, max),
        _ => target,
    }
}

/// The PipeWire sink QBZ suspended to take a device exclusively (ALSA-direct
/// EBUSY retry / CPAL-exclusive). Recorded by *resolved name* so
/// `resume_suspended_sink` wakes the exact sink that was suspended — PipeWire
/// sink names are deterministic, so resume-by-name is reliable even if the sink
/// was vacated and re-created while QBZ held the device. Issue #263: QBZ
/// suspended the default sink but never resumed it, leaving the rest of the
/// system stuck on a suspended sink after exclusive playback.
static SUSPENDED_SINK: Mutex<Option<String>> = Mutex::new(None);

// ReserveDevice1 can make WirePlumber vacate a sink without our pactl busy
// fallback ever running. Remember that sink before the reservation hides it.
static EXCLUSIVE_SINKS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn sinks_for_pcm(device: &str, inventory: &serde_json::Value) -> Vec<String> {
    let Some((_, args)) = device.split_once(':') else {
        return Vec::new();
    };
    let parts: Vec<_> = args.split(',').collect();
    let card = parts
        .iter()
        .find_map(|p| p.strip_prefix("CARD="))
        .or_else(|| parts.first().copied().filter(|p| !p.contains('=')));
    let pcm = parts
        .iter()
        .find_map(|p| p.strip_prefix("DEV="))
        .or_else(|| parts.get(1).copied().filter(|p| !p.contains('=')))
        .unwrap_or("0");
    let (Some(card), Ok(pcm)) = (card, pcm.parse::<u32>()) else {
        return Vec::new();
    };
    let Some(sinks) = inventory.as_array() else {
        return Vec::new();
    };
    sinks
        .iter()
        .filter_map(|sink| {
            let props = sink.get("properties")?;
            let card_matches = if let Ok(index) = card.parse::<u32>() {
                property_number(&props["alsa.card"]) == Some(index)
            } else {
                props["alsa.id"].as_str() == Some(card)
            };
            (card_matches && property_number(&props["alsa.device"]) == Some(pcm))
                .then(|| sink["name"].as_str().map(str::to_owned))
                .flatten()
        })
        .collect()
}

fn property_number(value: &serde_json::Value) -> Option<u32> {
    value
        .as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| value.as_u64().and_then(|n| n.try_into().ok()))
}

/// Capture the actual DAC, never the default output, before ReserveDevice1
/// can remove its sink. Failed PCM opens must not wake another owner's sink.
pub(crate) fn with_sink_recovery<T>(
    device: &str,
    open: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let targets = pactl_output(&["--format=json", "list", "sinks"])
        .map_err(|error| {
            log::warn!("[ALSA Backend] Cannot capture recovery target for '{device}': {error}")
        })
        .ok()
        .and_then(|output| serde_json::from_slice(&output).ok())
        .map(|inventory| sinks_for_pcm(device, &inventory))
        .unwrap_or_default();
    open_with_recovery(targets, open, |targets| {
        if let Ok(mut pending) = EXCLUSIVE_SINKS.lock() {
            for sink in targets {
                if !pending.contains(&sink) {
                    log::info!("[ALSA Backend] Recording sink '{sink}' for recovery after exclusive playback");
                    pending.push(sink);
                }
            }
        }
    })
}

fn open_with_recovery<T>(
    targets: Vec<String>,
    open: impl FnOnce() -> Result<T, String>,
    record: impl FnOnce(Vec<String>),
) -> Result<T, String> {
    let stream = open()?;
    record(targets);
    Ok(stream)
}

// Bound each daemon interaction so recovery can finish inside the shutdown
// watchdog. Drain stdout concurrently: sink inventories can exceed pipe capacity.
fn pactl_output(args: &[&str]) -> Result<Vec<u8>, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new("pactl")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_millis(500);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(match result {
                    Err(error) => error.to_string(),
                    _ => "pactl timed out after 500ms".into(),
                });
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| "pactl output reader panicked".to_string())?
        .map_err(|error| error.to_string())?;
    let status = status?;
    if status.success() {
        Ok(bytes)
    } else {
        Err(format!("pactl {args:?}: {status}"))
    }
}

/// Suspend the current default PipeWire sink so an exclusive ALSA device open
/// can grab the hardware, recording the resolved sink name for later resume.
/// Falls back to the `@DEFAULT_SINK@` alias if the name can't be resolved.
fn suspend_default_sink_for_exclusive() {
    let resolved = std::process::Command::new("pactl")
        .args(["get-default-sink"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let target = resolved.unwrap_or_else(|| "@DEFAULT_SINK@".to_string());
    match std::process::Command::new("pactl")
        .args(["suspend-sink", &target, "1"])
        .output()
    {
        Ok(output) if output.status.success() => {
            log::info!(
                "[ALSA Backend] Suspended PipeWire sink '{}' for exclusive access",
                target
            );
            if let Ok(mut guard) = SUSPENDED_SINK.lock() {
                *guard = Some(target);
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!("[ALSA Backend] Failed to suspend PipeWire sink: {}", stderr);
        }
        Err(e) => log::warn!("[ALSA Backend] Error suspending PipeWire sink: {}", e),
    }
}

/// Resume the PipeWire sink QBZ suspended for exclusive access (issue #263 leak
/// fix), including sinks vacated through ReserveDevice1. Safe on every
/// stop/teardown when no recovery is pending. Call it once the exclusive device has actually been released
/// so the rest of the system can use the sink again.
pub fn resume_suspended_sink() -> Result<(), String> {
    let mut targets = EXCLUSIVE_SINKS
        .lock()
        .map_err(|_| "sink recovery lock poisoned".to_string())?
        .clone();
    if let Some(sink) = SUSPENDED_SINK
        .lock()
        .map_err(|_| "suspended sink lock poisoned".to_string())?
        .clone()
    {
        if !targets.contains(&sink) {
            targets.push(sink);
        }
    }
    if targets.is_empty() {
        log::info!("[ALSA Backend] No recorded PipeWire sinks to recover");
        return Ok(());
    }
    let mut failures = Vec::new();
    for sink in targets {
        let result = resume_sink_with(&sink, |target| {
            pactl_output(&["suspend-sink", target, "0"]).map(|_| ())
        });
        match result {
            Ok(()) => {
                if let Ok(mut pending) = EXCLUSIVE_SINKS.lock() {
                    pending.retain(|target| target != &sink);
                }
                if let Ok(mut pending) = SUSPENDED_SINK.lock() {
                    if pending.as_deref() == Some(sink.as_str()) {
                        *pending = None;
                    }
                }
                log::info!("[ALSA Backend] PipeWire accepted recovery of sink '{sink}'");
            }
            Err(error) => failures.push(error),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn resume_sink_with(
    sink: &str,
    mut resume: impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    // Only success on the recorded sink counts. The default may now be HDMI
    // or another DAC; waking it must not discard the pending recovery target.
    let mut last_error = String::new();
    for delay_ms in [0u64, 25, 75, 150] {
        if delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
        match resume(sink) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
    }
    Err(format!(
        "cannot resume PipeWire sink '{sink}' after releasing ALSA: {last_error}"
    ))
}

#[cfg(test)]
mod sink_recovery_tests {
    use super::{open_with_recovery, resume_sink_with, sinks_for_pcm};

    fn inventory() -> serde_json::Value {
        serde_json::json!([
            {"name":"laptop", "properties":{"alsa.card":"0", "alsa.id":"PCH", "alsa.device":"0"}},
            {"name":"dac", "properties":{"alsa.card":"1", "alsa.id":"ZH3", "alsa.device":"0"}},
            {"name":"dac-other-pcm", "properties":{"alsa.card":1, "alsa.id":"ZH3", "alsa.device":1}}
        ])
    }

    #[test]
    fn captures_the_dac_by_card_and_pcm_not_the_laptop_output() {
        for device in ["hw:CARD=ZH3,DEV=0", "plughw:1,0", "hw:DEV=0,CARD=ZH3"] {
            assert_eq!(sinks_for_pcm(device, &inventory()), vec!["dac"]);
        }
        assert_eq!(sinks_for_pcm("hw:1,1", &inventory()), vec!["dac-other-pcm"]);
    }

    #[test]
    fn missing_identity_never_falls_back_to_an_unrelated_sink() {
        for device in ["default", "pulse", "hw:CARD=ABSENT,DEV=0", "hw:1,99"] {
            assert!(sinks_for_pcm(device, &inventory()).is_empty());
        }
        assert!(sinks_for_pcm("hw:1,0", &serde_json::json!([{"name":"unknown"}])).is_empty());
    }

    #[test]
    fn successful_first_open_records_recovery_without_a_busy_retry() {
        let mut recorded = Vec::new();
        let result = open_with_recovery(
            sinks_for_pcm("hw:1,0", &inventory()),
            || Ok("pcm"),
            |targets| recorded = targets,
        );
        assert_eq!(result.unwrap(), "pcm");
        assert_eq!(recorded, vec!["dac"]);
    }

    #[test]
    fn refused_open_does_not_schedule_recovery_of_another_owners_device() {
        let result: Result<(), String> = open_with_recovery(
            vec!["dac".into()],
            || Err("higher priority owner".into()),
            |_| panic!("must not record a failed open"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn another_default_sink_cannot_acknowledge_dac_recovery() {
        let mut attempts = Vec::new();
        let result = resume_sink_with("usb-dac", |target| {
            attempts.push(target.to_string());
            if target == "@DEFAULT_SINK@" {
                Ok(())
            } else {
                Err("not recreated yet".into())
            }
        });
        assert!(result.is_err());
        assert_eq!(attempts, vec!["usb-dac"; 4]);
    }

    #[test]
    fn retries_the_recorded_sink_until_it_reappears() {
        let mut attempts = 0;
        assert!(resume_sink_with("usb-dac", |target| {
            assert_eq!(target, "usb-dac");
            attempts += 1;
            if attempts == 2 {
                Ok(())
            } else {
                Err("not recreated yet".into())
            }
        })
        .is_ok());
        assert_eq!(attempts, 2);
    }
}

/// Common audio sample rates to check for device support
const COMMON_SAMPLE_RATES: &[u32] = &[
    44100,  // CD quality
    48000,  // DVD/DAT quality
    88200,  // 2x CD
    96000,  // DVD-Audio
    176400, // 4x CD
    192000, // High-res audio
    352800, // DSD64 equivalent
    384000, // Ultra high-res
];

/// Find the best fallback sample rate in the same family.
/// 44.1kHz family: 44100, 88200, 176400, 352800
/// 48kHz family: 48000, 96000, 192000, 384000
fn find_best_fallback_rate(requested: u32, supported: &[u32]) -> u32 {
    let is_441_family = requested % 44100 == 0;

    // Find highest supported rate in the same family that's <= requested
    let mut candidates: Vec<u32> = supported
        .iter()
        .filter(|&&r| {
            if is_441_family {
                r % 44100 == 0
            } else {
                r % 48000 == 0
            }
        })
        .filter(|&&r| r <= requested)
        .copied()
        .collect();
    candidates.sort();

    if let Some(&best) = candidates.last() {
        return best;
    }

    // No rate in the same family — use highest supported rate overall
    supported.iter().copied().max().unwrap_or(48000)
}

/// Return `true` when the given CPAL/ALSA PCM name matches one of the ID
/// shapes our `/proc/asound`-driven enumeration ever looks up.
///
/// Used by `build_cpal_device_map` to drop virtual PCMs (dmix, route,
/// surround*, pulse, null, …) whose probing only produces noise.
fn is_known_pcm_id(name: &str) -> bool {
    name == "default"
        || name.starts_with("sysdefault:CARD=")
        || name.starts_with("front:CARD=")
        || name.starts_with("hdmi:CARD=")
        || name.starts_with("iec958:CARD=")
}

/// Extract supported sample rates from a CPAL device
fn get_supported_sample_rates(device: &rodio::cpal::Device) -> Option<Vec<u32>> {
    use rodio::cpal::traits::DeviceTrait;

    let configs = device.supported_output_configs().ok()?;
    let configs_vec: Vec<_> = configs.collect();

    if configs_vec.is_empty() {
        return None;
    }

    let mut supported = Vec::new();

    for rate in COMMON_SAMPLE_RATES {
        let sample_rate = *rate;
        // Check if any config supports this rate
        let is_supported = configs_vec.iter().any(|config| {
            sample_rate >= config.min_sample_rate() && sample_rate <= config.max_sample_rate()
        });
        if is_supported {
            supported.push(*rate);
        }
    }

    if supported.is_empty() {
        None
    } else {
        Some(supported)
    }
}

// ============================================================================
// /proc/asound helpers - No aplay dependency
// ============================================================================

/// Information about an ALSA sound card read from /proc/asound
#[derive(Debug, Clone)]
struct ProcCardInfo {
    /// Card number (0, 1, 2, ...)
    number: String,
    /// Short name used in ALSA device IDs (e.g., "C20", "NVidia", "sofhdadsp")
    short_name: String,
    /// Long descriptive name (e.g., "Cambridge Audio USB Audio 2.0")
    long_name: String,
    /// PCM playback devices on this card
    pcm_playback_devices: Vec<ProcPcmInfo>,
}

/// Information about a PCM device
#[derive(Debug, Clone)]
struct ProcPcmInfo {
    /// Device number within the card
    device_num: String,
    /// Device name (e.g., "USB Audio", "HDMI 0")
    name: String,
}

/// Read all sound card information from /proc/asound
fn read_proc_asound_cards() -> Vec<ProcCardInfo> {
    let mut cards = Vec::new();

    // Parse /proc/asound/cards for basic card info
    // Format: " 0 [C20            ]: USB-Audio - Cambridge Audio USB Audio 2.0"
    let cards_content = match fs::read_to_string("/proc/asound/cards") {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[ALSA] Cannot read /proc/asound/cards: {}", e);
            return cards;
        }
    };

    // Parse cards file - each card has two lines
    let lines: Vec<&str> = cards_content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();

        // First line format: " 0 [C20            ]: USB-Audio - Cambridge Audio USB Audio 2.0"
        if let Some(card_info) = parse_proc_card_line(line) {
            // Read PCM devices for this card
            let pcm_devices = read_card_pcm_devices(&card_info.0);

            cards.push(ProcCardInfo {
                number: card_info.0,
                short_name: card_info.1,
                long_name: card_info.2,
                pcm_playback_devices: pcm_devices,
            });
        }
        i += 1;
    }

    cards
}

/// Parse a line from /proc/asound/cards
/// Returns (card_number, short_name, long_name)
fn parse_proc_card_line(line: &str) -> Option<(String, String, String)> {
    // Format: " 0 [C20            ]: USB-Audio - Cambridge Audio USB Audio 2.0"
    let line = line.trim();

    // Find card number (first number)
    let parts: Vec<&str> = line.splitn(2, '[').collect();
    if parts.len() < 2 {
        return None;
    }

    let card_num = parts[0].trim().to_string();
    if card_num.is_empty() || !card_num.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    // Find short name (inside brackets)
    let rest = parts[1];
    let bracket_end = rest.find(']')?;
    let short_name = rest[..bracket_end].trim().to_string();

    // Find long name (after " - ")
    let long_name = if let Some(dash_pos) = rest.find(" - ") {
        rest[dash_pos + 3..].trim().to_string()
    } else {
        // Fallback: use everything after ]:
        rest[bracket_end + 1..]
            .trim()
            .trim_start_matches(':')
            .trim()
            .split(" - ")
            .last()
            .unwrap_or(&short_name)
            .to_string()
    };

    Some((card_num, short_name, long_name))
}

/// Read PCM playback devices for a specific card from /proc/asound
fn read_card_pcm_devices(card_num: &str) -> Vec<ProcPcmInfo> {
    let mut devices = Vec::new();
    let card_path = format!("/proc/asound/card{}", card_num);

    // Read PCM device info files
    if let Ok(entries) = fs::read_dir(&card_path) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // PCM playback devices are named pcmXp (X = device number, p = playback)
            if name_str.starts_with("pcm") && name_str.ends_with('p') {
                let info_path = entry.path().join("info");
                if let Ok(content) = fs::read_to_string(&info_path) {
                    let mut pcm_name = String::new();
                    let mut device_num = String::new();

                    for line in content.lines() {
                        if let Some(val) = line.strip_prefix("name: ") {
                            pcm_name = val.trim().to_string();
                        }
                        if let Some(val) = line.strip_prefix("device: ") {
                            device_num = val.trim().to_string();
                        }
                    }

                    if !device_num.is_empty() {
                        devices.push(ProcPcmInfo {
                            device_num,
                            name: if pcm_name.is_empty() {
                                "Unknown".to_string()
                            } else {
                                pcm_name
                            },
                        });
                    }
                }
            }
        }
    }

    // Sort by device number
    devices.sort_by(|a, b| {
        a.device_num
            .parse::<u32>()
            .unwrap_or(0)
            .cmp(&b.device_num.parse::<u32>().unwrap_or(0))
    });

    devices
}

/// Build a map of card_number -> (short_name, long_name) from /proc/asound
fn build_card_info_map() -> HashMap<String, (String, String)> {
    let cards = read_proc_asound_cards();
    let mut map = HashMap::new();

    for card in cards {
        map.insert(card.number.clone(), (card.short_name, card.long_name));
    }

    map
}

/// Find card number by short name (e.g., "C20" -> "0")
fn find_card_number_by_name(short_name: &str) -> Option<String> {
    let cards = read_proc_asound_cards();
    cards
        .iter()
        .find(|c| c.short_name == short_name)
        .map(|c| c.number.clone())
}

/// Build a `hw:CARD=<name>,DEV=<n>` fallback id from an aliased device id.
/// Returns None for ids that don't carry a `CARD=<name>,DEV=<n>` shape
/// (e.g. `default`, `hw:0,0`, unknown formats).
///
/// The raw `hw:` PCM is defined by the kernel driver for every card the
/// system lists in /proc/asound, which makes it a safe last-resort when
/// higher-level aliases like `iec958:` or `front:` aren't declared in the
/// user's asound.conf (issue #331 — minimal Raspberry Pi OS installs don't
/// ship a config entry for `iec958:CARD=<name>`, so the HifiBerry Digi2
/// Pro's only selectable id failed to open even though the card was
/// present and usable via `hw:`).
fn build_hw_fallback_id(device_id: &str) -> Option<String> {
    if !(device_id.starts_with("front:CARD=")
        || device_id.starts_with("sysdefault:CARD=")
        || device_id.starts_with("iec958:CARD=")
        || device_id.starts_with("hdmi:CARD="))
    {
        return None;
    }
    let after_card = device_id.split("CARD=").nth(1)?;
    let mut parts = after_card.splitn(2, ',');
    let card_name = parts.next()?.to_string();
    let dev_part = parts.next().unwrap_or("DEV=0");
    let dev_num = dev_part.strip_prefix("DEV=").unwrap_or("0");
    Some(format!("hw:CARD={},DEV={}", card_name, dev_num))
}

/// Derive the raw `(hw, plughw)` open ids for an aliased device id.
///
/// Aliases like `front:` are resolved by alsa-lib through per-card config
/// (`cards.<driver>.pcm.front.<DEV>`), which some cards don't declare for
/// every device — snd-aloop's `Loopback.conf` defines `front` for DEV=0
/// only, and there it routes through softvol — so opening the alias itself
/// can fail with ENOENT at config-expansion time, or add an unwanted plugin
/// stage (discussion #641). The raw ids take the same `CARD=`/`DEV=` args
/// directly from the kernel driver, with no config dependency.
///
/// Returns None for ids that carry no `CARD=` alias shape (raw `hw:`,
/// `default`, etc.) — the caller keeps its existing handling for those.
fn raw_open_ids(device_id: &str) -> Option<(String, String)> {
    let hw = build_hw_fallback_id(device_id)?;
    let plug = hw.replacen("hw:", "plughw:", 1);
    Some((hw, plug))
}

/// Extract card name from an ALSA device ID.
/// `front:CARD=C20,DEV=0` -> `"C20"`, `hw:0,0` -> card 0 short name, etc.
///
/// Handles every alias shape `enumerate_with_proc_descriptions` produces:
/// `front:CARD=`, `sysdefault:CARD=`, `iec958:CARD=`, `hdmi:CARD=`, and the
/// raw `hw:N,M` / `plughw:N,M` forms. Missing any of these meant the
/// device-not-found error message downgraded to the generic "disconnected,
/// renamed, or handled by another app" wording for S/PDIF / HDMI devices
/// (issue #331 — HifiBerry Digi2 Pro is S/PDIF-only, so its selected id is
/// `iec958:CARD=sndrpihifiberry,DEV=0`).
fn extract_card_name_from_device(device_id: &str) -> Option<String> {
    if device_id.starts_with("front:CARD=")
        || device_id.starts_with("sysdefault:CARD=")
        || device_id.starts_with("iec958:CARD=")
        || device_id.starts_with("hdmi:CARD=")
    {
        // <prefix>:CARD=<name>,DEV=<n> -> <name>
        let after_card = device_id.split("CARD=").nth(1)?;
        Some(after_card.split(',').next()?.to_string())
    } else if device_id.starts_with("hw:") || device_id.starts_with("plughw:") {
        // hw:0,0 -> card number 0 -> look up short name
        let prefix = if device_id.starts_with("hw:") {
            "hw:"
        } else {
            "plughw:"
        };
        let card_num = device_id.strip_prefix(prefix)?.split(',').next()?;
        let cards = read_proc_asound_cards();
        cards
            .iter()
            .find(|c| c.number == card_num)
            .map(|c| c.short_name.clone())
    } else {
        None
    }
}

/// Check whether the card referenced by an ALSA device id is registered
/// in `/proc/asound/cards`. Used as a presence gate before attempting the
/// `hw:CARD=…,DEV=…` fallback in `create_output_stream` — we only want to
/// retry against the raw kernel PCM when the card actually exists.
///
/// Distinct from `get_hw_supported_rates`: that one parses
/// `/proc/asound/cardN/stream0`, which is only emitted by the USB-audio
/// driver. I2S / PCI / built-in cards (HifiBerry, intel-hda, etc.) don't
/// have a stream0 file at all, so using rate-readability as a presence
/// check made the fallback a no-op for the very devices that needed it
/// (issue #331 — HifiBerry Digi2 Pro on Raspberry Pi OS).
fn is_card_present_in_proc(device_id: &str) -> bool {
    extract_card_name_from_device(device_id)
        .and_then(|card| find_card_number_by_name(&card))
        .is_some()
}

/// Read hardware-supported sample rates from /proc/asound/cardN/stream0.
/// Returns None if rates cannot be determined (treat as "try anyway").
fn get_hw_supported_rates(card_name: &str) -> Option<Vec<u32>> {
    let card_num = find_card_number_by_name(card_name)?;
    let stream_path = format!("/proc/asound/card{}/stream0", card_num);
    let content = fs::read_to_string(&stream_path).ok()?;

    let mut in_playback = false;
    let mut all_rates = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "Playback:" {
            in_playback = true;
        } else if trimmed == "Capture:" {
            in_playback = false;
        }
        if in_playback && trimmed.starts_with("Rates:") {
            let rates_str = trimmed.trim_start_matches("Rates:").trim();
            if rates_str.contains("continuous") {
                return None; // Any rate supported — don't restrict
            }
            for rate_str in rates_str.split(',') {
                if let Ok(rate) = rate_str.trim().parse::<u32>() {
                    if !all_rates.contains(&rate) {
                        all_rates.push(rate);
                    }
                }
            }
        }
    }

    if all_rates.is_empty() {
        None
    } else {
        all_rates.sort();
        Some(all_rates)
    }
}

// ============================================================================
// ALSA Backend Implementation
// ============================================================================

pub struct AlsaBackend {
    host: rodio::cpal::Host,
}

impl AlsaBackend {
    pub fn new() -> BackendResult<Self> {
        // Try to get ALSA host
        let available_hosts = rodio::cpal::available_hosts();

        // Check if ALSA is available
        // cpal 0.17 changed HostId::name() from "ALSA" to "Alsa" (uses stringify!)
        if !available_hosts
            .iter()
            .any(|h| h.name().eq_ignore_ascii_case("alsa"))
        {
            return Err("ALSA host not available on this system".to_string());
        }

        // Get ALSA host
        let host = rodio::cpal::host_from_id(
            available_hosts
                .into_iter()
                .find(|h| h.name().eq_ignore_ascii_case("alsa"))
                .ok_or("ALSA host not found".to_string())?,
        )
        .map_err(|e| format!("Failed to create ALSA host: {}", e))?;

        log::info!("[ALSA Backend] Initialized successfully");

        Ok(Self { host })
    }

    /// Enumerate ALSA devices using /proc/asound as PRIMARY source
    ///
    /// This approach ensures consistent device enumeration regardless of playback state.
    /// CPAL enumeration fails when devices are in exclusive mode, but /proc/asound
    /// always sees all devices.
    ///
    /// Architecture:
    /// 1. /proc/asound = PRIMARY source (always complete)
    /// 2. CPAL = OPTIONAL enrichment (sample rates only, may fail during playback)
    fn enumerate_with_proc_descriptions(&self) -> BackendResult<Vec<AudioDevice>> {
        let mut devices = Vec::new();

        // Read all cards from /proc/asound (PRIMARY SOURCE)
        let cards = read_proc_asound_cards();

        log::info!("[ALSA Backend] /proc/asound found {} cards", cards.len());
        for card in &cards {
            log::debug!(
                "[ALSA Backend] Card {}: {} = {} ({} PCM devices)",
                card.number,
                card.short_name,
                card.long_name,
                card.pcm_playback_devices.len()
            );
        }

        // Build CPAL device map for sample rate enrichment (OPTIONAL - may be incomplete)
        let cpal_devices = self.build_cpal_device_map();
        log::debug!(
            "[ALSA Backend] CPAL found {} devices for enrichment",
            cpal_devices.len()
        );

        // Add system default device
        let default_sample_rates = cpal_devices
            .get("default")
            .and_then(|d| get_supported_sample_rates(d));
        let default_max_rate = default_sample_rates
            .as_ref()
            .and_then(|rates| rates.iter().max().copied());

        devices.push(AudioDevice {
            id: "default".to_string(),
            name: "default".to_string(),
            description: None, // Frontend shows "System Default"
            is_default: true,
            max_sample_rate: default_max_rate.or(Some(384000)),
            supported_sample_rates: default_sample_rates,
            device_bus: None,
            is_hardware: false,
        });

        // For each card, add relevant devices using STABLE IDs (card NAME, not number)
        for card in &cards {
            // Skip cards with no PCM playback devices. /proc/asound/cards lists
            // every registered sound card, including capture-only hardware
            // (USB webcams, microphones, HDMI-audio-less capture devices).
            // Those expose only `pcmXc` (capture) nodes, so `read_card_pcm_devices`
            // — which keeps only `pcmXp` (playback) — yields an empty list for
            // them. Without this guard such a card still got a bogus
            // `sysdefault:CARD=<name>` output entry, e.g. a webcam showing up as
            // a selectable "output device".
            if card.pcm_playback_devices.is_empty() {
                log::debug!(
                    "[ALSA Backend] Skipping capture-only card {} ({}) — no playback PCMs",
                    card.number,
                    card.short_name
                );
                continue;
            }

            // Add sysdefault:CARD=name (card default with software mixing)
            let sysdefault_id = format!("sysdefault:CARD={}", card.short_name);
            let sysdefault_rates = cpal_devices
                .get(&sysdefault_id)
                .and_then(|d| get_supported_sample_rates(d));

            devices.push(AudioDevice {
                id: sysdefault_id.clone(),
                name: sysdefault_id.clone(),
                description: Some(format!("{}, {}", card.long_name, sysdefault_id)),
                is_default: false,
                max_sample_rate: sysdefault_rates
                    .as_ref()
                    .and_then(|r| r.iter().max().copied())
                    .or(Some(192000)),
                supported_sample_rates: sysdefault_rates,
                device_bus: None,
                is_hardware: false, // sysdefault uses dmix
            });

            // Add PCM-specific devices (front:, iec958:, hdmi:)
            for pcm in &card.pcm_playback_devices {
                // Determine device type based on PCM name
                let device_prefix = if pcm.name.to_lowercase().contains("hdmi") {
                    "hdmi"
                } else if pcm.name.to_lowercase().contains("iec958")
                    || pcm.name.to_lowercase().contains("spdif")
                    || pcm.name.to_lowercase().contains("s/pdif")
                {
                    "iec958"
                } else {
                    "front" // Default to front: for analog/USB audio
                };

                let device_id = format!(
                    "{}:CARD={},DEV={}",
                    device_prefix, card.short_name, pcm.device_num
                );

                // Skip if already added (shouldn't happen, but be safe)
                if devices.iter().any(|d| d.id == device_id) {
                    continue;
                }

                // Try to get sample rates from CPAL (may fail if device is busy)
                let sample_rates = cpal_devices
                    .get(&device_id)
                    .and_then(|d| get_supported_sample_rates(d));
                let max_rate = sample_rates
                    .as_ref()
                    .and_then(|r| r.iter().max().copied())
                    .or(Some(384000)); // Assume high capability if CPAL unavailable

                devices.push(AudioDevice {
                    id: device_id.clone(),
                    name: device_id.clone(),
                    description: Some(format!("{}, {}", card.long_name, device_id)),
                    is_default: false,
                    max_sample_rate: max_rate,
                    supported_sample_rates: sample_rates,
                    device_bus: None,
                    is_hardware: true,
                });
            }
        }

        log::debug!("[ALSA Backend] Enumerated {} ALSA devices", devices.len());
        for (idx, dev) in devices.iter().enumerate() {
            log::debug!(
                "  [{}] {} - {} (max_rate: {:?}, rates: {:?})",
                idx,
                dev.name,
                dev.description.as_deref().unwrap_or("(default)"),
                dev.max_sample_rate,
                dev.supported_sample_rates
            );
        }

        Ok(devices)
    }

    /// Build a map of device_id -> CPAL Device for sample rate queries.
    /// This is OPTIONAL enrichment — devices may be missing if in exclusive use.
    ///
    /// Only the PCM name patterns we actually look up downstream are kept:
    /// `default`, `sysdefault:CARD=…`, and `{front,hdmi,iec958}:CARD=…,DEV=…`.
    /// Virtual PCMs (`dmix:`, `route:`, `surround51:` and the like) are
    /// dropped — we never query them, and letting them reach a later
    /// `supported_output_configs()` call just invites spurious libasound
    /// errors ("unable to open slave", "no matching channel map") on systems
    /// where PipeWire or another client holds the underlying hardware.
    fn build_cpal_device_map(&self) -> HashMap<String, rodio::cpal::Device> {
        let mut map = HashMap::new();

        if let Ok(output_devices) = self.host.output_devices() {
            for device in output_devices {
                if let Ok(description) = device.description() {
                    let name = description.name().to_string();
                    if is_known_pcm_id(&name) {
                        map.insert(name, device);
                    }
                }
            }
        }

        map
    }

    /// Try to create direct ALSA stream for hw: devices (bypasses CPAL)
    /// Returns None if device is not a hw: device (should use CPAL instead)
    ///
    /// Implements controlled fallback:
    /// 1. Try direct hw access first
    /// 2. If format unsupported, try plughw (format conversion only, no resampling)
    /// 3. Abort on other errors (busy, permissions, etc.)
    pub fn try_create_direct_stream(
        &self,
        config: &BackendConfig,
    ) -> Option<Result<(super::AlsaDirectStream, super::backend::BitPerfectMode), String>> {
        let device_id = config.device_id.as_ref()?;

        // Only use direct ALSA for hw:/plughw:/front: devices
        if !super::AlsaDirectStream::is_hw_device(device_id) {
            log::info!(
                "[ALSA Backend] Device '{}' is not hw:/plughw:/front:, using CPAL",
                device_id
            );
            return None;
        }

        // Determine the base device path for hw/plughw attempts. Aliased ids
        // (front:CARD=…) are opened through their raw hw:/plughw: forms: the
        // alias itself may be undeclared for the selected DEV (snd-aloop only
        // declares `front` for DEV=0) and needlessly routes through alsa-lib
        // plugins, while the raw ids open the kernel PCM directly (#641).
        let (hw_device, plughw_device) = if let Some(ids) = raw_open_ids(device_id) {
            ids
        } else if device_id.starts_with("hw:") {
            (device_id.to_string(), device_id.replace("hw:", "plughw:"))
        } else if device_id.starts_with("plughw:") {
            // Already plughw, try it directly
            (device_id.replace("plughw:", "hw:"), device_id.to_string())
        } else {
            (device_id.to_string(), format!("plug:{}", device_id))
        };

        // Pre-check: read /proc/asound/cardN/stream0 to verify rate support
        // before opening the PCM device. This avoids the "device busy" issue
        // where ALSA Direct opens the device, fails to configure the rate, and
        // leaves the device in a state CPAL can't open afterwards.
        //
        // When the rate is unsupported, we skip the hw: attempt and fall through
        // to the plughw: path below, which lets ALSA auto-resample in kernel/
        // userspace. Falling back to CPAL is not an option because CPAL's device
        // enumeration does not reliably expose raw hw: devices by the same name
        // the UI stored (front:CARD=X,DEV=Y vs hw:CARD=X,DEV=Y), and the lookup
        // fails with a misleading "Device not found" error. See issue #288.
        let mut hw_rate_unsupported = false;
        if let Some(card_name) = extract_card_name_from_device(device_id) {
            if let Some(hw_rates) = get_hw_supported_rates(&card_name) {
                if !hw_rates.contains(&config.sample_rate) {
                    log::info!(
                        "[ALSA Backend] Hardware rates for '{}': {:?}. {}Hz not supported natively — falling back to plughw for software resample",
                        card_name, hw_rates, config.sample_rate
                    );
                    hw_rate_unsupported = true;
                } else {
                    log::info!(
                        "[ALSA Backend] Hardware confirms support for {}Hz (card '{}', rates: {:?})",
                        config.sample_rate,
                        card_name,
                        hw_rates
                    );
                }
            }
        }

        // Respect ALSA plugin selection from settings. When /proc/asound already
        // told us the hw device won't accept the rate, skip straight to plughw
        // even if the user prefers Hw — plughw with resample is the only path
        // that produces sound.
        let try_hw_first = match config.alsa_plugin {
            Some(AlsaPlugin::Hw) => !hw_rate_unsupported,
            Some(AlsaPlugin::PlugHw) => false, // Skip hw, go directly to plughw
            Some(AlsaPlugin::Pcm) => {
                log::info!("[ALSA Backend] PCM mode selected, not using direct ALSA");
                return None; // Use CPAL instead
            }
            None => !hw_rate_unsupported, // Default: try hw first if rate is supported
        };

        if try_hw_first {
            log::info!(
                "[ALSA Backend] Attempting DIRECT hw stream: {} ({}Hz, {}ch)",
                hw_device,
                config.sample_rate,
                config.channels
            );

            match super::AlsaDirectStream::new(&hw_device, config.sample_rate, config.channels) {
                Ok(stream) => {
                    log::info!("[ALSA Backend] ✓ Direct hw stream created successfully");
                    return Some(Ok((stream, super::backend::BitPerfectMode::DirectHardware)));
                }
                Err(e) => {
                    let error = super::backend::AlsaDirectError::from_alsa_error(&e);
                    log::warn!("[ALSA Backend] hw attempt failed: {}", error);

                    if matches!(error, super::backend::AlsaDirectError::DeviceBusy(_)) {
                        // Device busy: either our own previous PCM handle is still
                        // releasing (race on fast track skip) or PipeWire holds it.
                        // Retry with progressive backoff before giving up.
                        log::info!("[ALSA Backend] Device busy — retrying with backoff");

                        // Try suspending PipeWire once (covers PipeWire-held case).
                        // Records the sink so it is resumed on stop/teardown (#263).
                        suspend_default_sink_for_exclusive();

                        let retry_delays_ms = [50, 100, 200, 400, 800];
                        for (i, delay_ms) in retry_delays_ms.iter().enumerate() {
                            std::thread::sleep(std::time::Duration::from_millis(*delay_ms));

                            match super::AlsaDirectStream::new(
                                &hw_device,
                                config.sample_rate,
                                config.channels,
                            ) {
                                Ok(stream) => {
                                    log::info!(
                                        "[ALSA Backend] ✓ Direct hw stream created on retry {} (after {}ms)",
                                        i + 1,
                                        delay_ms
                                    );
                                    return Some(Ok((
                                        stream,
                                        super::backend::BitPerfectMode::DirectHardware,
                                    )));
                                }
                                Err(e2) => {
                                    log::warn!(
                                        "[ALSA Backend] Retry {}/{} failed: {}",
                                        i + 1,
                                        retry_delays_ms.len(),
                                        e2
                                    );
                                }
                            }
                        }

                        log::error!(
                            "[ALSA Backend] Cannot acquire device after {} retries",
                            retry_delays_ms.len()
                        );
                        return Some(Err(format!(
                            "ALSA Direct failed: {}. Device may be in use or inaccessible.",
                            error
                        )));
                    }

                    if matches!(error, super::backend::AlsaDirectError::InvalidParams(_)) {
                        // Hardware doesn't support this rate/format natively.
                        // Return None to let the player fall back to CPAL/rodio
                        // which can resample (e.g. 176.4kHz → 88.2kHz).
                        // Brief delay: ALSA Direct opened the PCM (then failed to configure it).
                        // The kernel needs a moment to fully release the device before CPAL can open it.
                        log::info!(
                            "[ALSA Backend] Hardware doesn't support {}Hz natively, releasing device for CPAL fallback",
                            config.sample_rate
                        );
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        return None;
                    }

                    if !error.allows_plughw_fallback() {
                        // Non-recoverable error (permissions, etc.)
                        log::error!("[ALSA Backend] Cannot fallback - error type: {:?}", error);
                        return Some(Err(format!(
                            "ALSA Direct failed: {}. Device may be in use or inaccessible.",
                            error
                        )));
                    }

                    log::info!(
                        "[ALSA Backend] Format unsupported on hw, trying plughw fallback..."
                    );
                }
            }
        }

        // Try plughw fallback (format conversion only)
        log::info!(
            "[ALSA Backend] Attempting plughw stream: {} ({}Hz, {}ch)",
            plughw_device,
            config.sample_rate,
            config.channels
        );

        match super::AlsaDirectStream::new(&plughw_device, config.sample_rate, config.channels) {
            Ok(stream) => {
                log::info!(
                    "[ALSA Backend] ✓ plughw stream created (bit-perfect with format conversion)"
                );
                Some(Ok((stream, super::backend::BitPerfectMode::PluginFallback)))
            }
            Err(e) => {
                let error = super::backend::AlsaDirectError::from_alsa_error(&e);

                if matches!(error, super::backend::AlsaDirectError::DeviceBusy(_)) {
                    log::info!("[ALSA Backend] plughw device busy — retrying with backoff");
                    suspend_default_sink_for_exclusive();

                    let retry_delays_ms = [50, 100, 200, 400, 800];
                    for (i, delay_ms) in retry_delays_ms.iter().enumerate() {
                        std::thread::sleep(std::time::Duration::from_millis(*delay_ms));

                        match super::AlsaDirectStream::new(
                            &plughw_device,
                            config.sample_rate,
                            config.channels,
                        ) {
                            Ok(stream) => {
                                log::info!(
                                    "[ALSA Backend] ✓ plughw stream created on retry {} (after {}ms)",
                                    i + 1,
                                    delay_ms
                                );
                                return Some(Ok((
                                    stream,
                                    super::backend::BitPerfectMode::PluginFallback,
                                )));
                            }
                            Err(e2) => {
                                log::warn!(
                                    "[ALSA Backend] plughw retry {}/{} failed: {}",
                                    i + 1,
                                    retry_delays_ms.len(),
                                    e2
                                );
                            }
                        }
                    }
                }

                if matches!(error, super::backend::AlsaDirectError::InvalidParams(_)) {
                    // Hardware doesn't support this rate even via plughw.
                    // Return None to let the player fall back to CPAL/rodio
                    // which can resample (e.g. 176.4kHz → 88.2kHz).
                    log::info!(
                        "[ALSA Backend] Hardware doesn't support {}Hz even via plughw, releasing device for CPAL fallback",
                        config.sample_rate
                    );
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    return None;
                }

                log::error!("[ALSA Backend] plughw fallback also failed: {}", e);
                Some(Err(format!(
                    "Bit-perfect playback could not be established. hw failed, plughw failed: {}",
                    e
                )))
            }
        }
    }
}

/// Convert an unstable hw:X,0 device ID to a stable front:CARD=name,DEV=0 format.
/// This survives reboots and USB reconnections since it uses the card name, not the number.
///
/// Examples:
/// - `hw:0,0` with card "C20" -> `front:CARD=C20,DEV=0`
/// - `hw:2,0` with card "NVidia" -> `front:CARD=NVidia,DEV=0`
/// - `front:CARD=C20,DEV=0` -> unchanged (already stable)
/// - `plughw:0,0` -> unchanged (plugin devices don't benefit from this)
/// - `default` -> unchanged (not a hardware device)
pub fn normalize_device_id_to_stable(device_id: &str) -> String {
    // Already stable formats - return as-is
    if device_id.starts_with("front:CARD=")
        || device_id.starts_with("plughw:")
        || !device_id.starts_with("hw:")
    {
        return device_id.to_string();
    }

    // Parse hw:X,Y format
    let stripped = device_id.strip_prefix("hw:").unwrap_or(device_id);
    let parts: Vec<&str> = stripped.split(',').collect();
    if parts.len() < 2 {
        log::warn!("[ALSA] Could not parse hw device format: {}", device_id);
        return device_id.to_string();
    }

    let card_num = parts[0];
    let device_num = parts[1];

    // Get card info from /proc/asound
    let card_map = build_card_info_map();

    if let Some((short_name, _long_name)) = card_map.get(card_num) {
        let stable_id = format!("front:CARD={},DEV={}", short_name, device_num);
        log::info!(
            "[ALSA] Normalized device ID: {} -> {} (stable)",
            device_id,
            stable_id
        );
        return stable_id;
    }

    log::warn!(
        "[ALSA] Could not find card {} in /proc/asound, keeping original ID",
        card_num
    );
    device_id.to_string()
}

/// Get the current card number for a stable device ID.
/// Used when we need to resolve front:CARD=X to hw:N,0 for certain operations.
///
/// Returns None if the card is not currently present.
pub fn resolve_stable_to_current_hw(device_id: &str) -> Option<String> {
    // Only resolve front:CARD= format
    if !device_id.starts_with("front:CARD=") {
        return Some(device_id.to_string());
    }

    // Extract card name: front:CARD=C20,DEV=0 -> C20
    let stripped = device_id.strip_prefix("front:CARD=")?;
    let parts: Vec<&str> = stripped.split(',').collect();
    let card_name = parts.first()?;
    let dev_part = parts
        .get(1)
        .and_then(|s| s.strip_prefix("DEV="))
        .unwrap_or("0");

    // Find current card number for this name using /proc/asound
    if let Some(card_num) = find_card_number_by_name(card_name) {
        let hw_id = format!("hw:{},{}", card_num, dev_part);
        log::debug!("[ALSA] Resolved {} -> {}", device_id, hw_id);
        return Some(hw_id);
    }

    log::warn!(
        "[ALSA] Card '{}' not found in current enumeration",
        card_name
    );
    None
}

/// Check if a hardware device supports a given sample rate.
/// Returns `Some(true)` if supported, `Some(false)` if not, `None` if unknown.
/// Uses /proc/asound/cardN/stream0 for accurate hardware capabilities.
pub fn device_supports_sample_rate(device_id: &str, sample_rate: u32) -> Option<bool> {
    let card_name = extract_card_name_from_device(device_id)?;
    let hw_rates = get_hw_supported_rates(&card_name)?;
    Some(hw_rates.contains(&sample_rate))
}

/// Get the hardware-supported sample rates for a device.
/// Returns None if rates cannot be determined.
pub fn get_device_supported_rates(device_id: &str) -> Option<Vec<u32>> {
    let card_name = extract_card_name_from_device(device_id)?;
    get_hw_supported_rates(&card_name)
}

/// Create a DoP-capable ALSA direct stream (DSD plan Phase 2). ADDITIVE
/// helper next to the protected PCM path: exclusive `hw:` open, S32_LE only,
/// exact carrier rate, `/proc` rate pre-check, and the same busy backoff +
/// PipeWire-suspend dance as the PCM path. Errors are user-meaningful (the
/// caller falls back to DSD→PCM conversion with a toast).
/// Create a NATIVE-DSD ALSA direct stream (DSD plan Phase 3). Same shape as
/// [`create_dop_stream`]: hw-only, busy backoff. No /proc rate pre-check —
/// the DSD altset rates aren't reliably visible there; the open itself
/// validates (and fails cleanly when the kernel quirk is missing).
/// Returns the stream plus `little_endian` for the packer.
pub fn create_native_dsd_stream(
    device_id: &str,
    dsd_rate: u32,
    channels: u16,
) -> Result<(super::AlsaDirectStream, bool), String> {
    if !super::AlsaDirectStream::is_hw_device(device_id) {
        return Err(format!(
            "device '{}' is not a direct hardware (hw:) device",
            device_id
        ));
    }
    let hw_device = if device_id.starts_with("plughw:") {
        device_id.replace("plughw:", "hw:")
    } else {
        device_id.to_string()
    };
    match super::AlsaDirectStream::new_native_dsd(&hw_device, dsd_rate, channels) {
        Ok(pair) => Ok(pair),
        Err(first_err) => {
            let error = super::backend::AlsaDirectError::from_alsa_error(&first_err);
            if !matches!(error, super::backend::AlsaDirectError::DeviceBusy(_)) {
                return Err(first_err);
            }
            log::info!("[ALSA Backend] Native-DSD device busy — retrying with backoff");
            suspend_default_sink_for_exclusive();
            let mut last_err = first_err;
            for delay_ms in [50u64, 100, 200, 400, 800] {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                match super::AlsaDirectStream::new_native_dsd(&hw_device, dsd_rate, channels) {
                    Ok(pair) => return Ok(pair),
                    Err(e) => last_err = e,
                }
            }
            Err(last_err)
        }
    }
}

pub fn create_dop_stream(
    device_id: &str,
    carrier_rate: u32,
    channels: u16,
) -> Result<super::AlsaDirectStream, String> {
    if !super::AlsaDirectStream::is_hw_device(device_id) {
        return Err(format!(
            "device '{}' is not a direct hardware (hw:) device",
            device_id
        ));
    }
    // Same base-id mapping as the PCM direct path (plughw is useless for DoP —
    // any conversion breaks the markers, so only the raw hw id is tried).
    let hw_device = if device_id.starts_with("plughw:") {
        device_id.replace("plughw:", "hw:")
    } else {
        device_id.to_string()
    };
    if let Some(card_name) = extract_card_name_from_device(device_id) {
        if let Some(hw_rates) = get_hw_supported_rates(&card_name) {
            if !hw_rates.contains(&carrier_rate) {
                return Err(format!(
                    "device does not support the {} Hz DoP carrier rate (supported: {:?})",
                    carrier_rate, hw_rates
                ));
            }
        }
    }
    match super::AlsaDirectStream::new_dop(&hw_device, carrier_rate, channels) {
        Ok(stream) => Ok(stream),
        Err(first_err) => {
            let error = super::backend::AlsaDirectError::from_alsa_error(&first_err);
            if !matches!(error, super::backend::AlsaDirectError::DeviceBusy(_)) {
                return Err(first_err);
            }
            log::info!("[ALSA Backend] DoP device busy — retrying with backoff");
            suspend_default_sink_for_exclusive();
            let mut last_err = first_err;
            for delay_ms in [50u64, 100, 200, 400, 800] {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                match super::AlsaDirectStream::new_dop(&hw_device, carrier_rate, channels) {
                    Ok(stream) => return Ok(stream),
                    Err(e) => last_err = e,
                }
            }
            Err(last_err)
        }
    }
}

impl AudioBackend for AlsaBackend {
    fn backend_type(&self) -> AudioBackendType {
        AudioBackendType::Alsa
    }

    fn enumerate_devices(&self) -> BackendResult<Vec<AudioDevice>> {
        self.enumerate_with_proc_descriptions()
    }

    fn create_output_stream(&self, config: &BackendConfig) -> BackendResult<MixerDeviceSink> {
        log::info!(
            "[ALSA Backend] Creating stream: {}Hz, {} channels, exclusive: {}, plugin: {:?}",
            config.sample_rate,
            config.channels,
            config.exclusive_mode,
            config.alsa_plugin
        );

        // Find the device by name/id.
        //
        // If /proc/asound shows this device exists but CPAL's enumeration
        // cannot match it, the app stored a name format CPAL does not expose
        // (e.g. front:CARD=X,DEV=Y when CPAL only yields hw:CARD=X,DEV=Y for
        // the raw device). Surface that distinction in the error so users
        // don't chase ghosts wondering why their DAC "disappeared".
        let device = if let Some(device_id) = &config.device_id {
            log::info!("[ALSA Backend] Looking for device: {}", device_id);
            let primary = self
                .host
                .output_devices()
                .map_err(|e| format!("Failed to enumerate devices: {}", e))?
                .find(|d| {
                    d.description()
                        .ok()
                        .map(|desc| desc.name() == device_id.as_str())
                        .unwrap_or(false)
                });

            // Fallback: if the primary alias didn't resolve but /proc/asound
            // shows the card is present, retry with the raw hw:CARD=<name>,
            // DEV=<n> PCM. The kernel driver always exposes that for any
            // registered card, which covers minimal distro configs where
            // iec958:/hdmi:/front: aliases aren't declared in asound.conf
            // (issue #331 — HifiBerry Digi2 Pro on Raspberry Pi OS).
            let resolved = match primary {
                Some(d) => Some(d),
                None => match build_hw_fallback_id(device_id) {
                    Some(hw_id) if is_card_present_in_proc(device_id) => {
                        log::warn!(
                            "[ALSA Backend] '{}' not resolvable by ALSA (alias likely missing in asound.conf); trying fallback '{}'",
                            device_id,
                            hw_id
                        );
                        let found = self
                            .host
                            .output_devices()
                            .map_err(|e| format!("Failed to enumerate devices: {}", e))?
                            .find(|d| {
                                d.description()
                                    .ok()
                                    .map(|desc| desc.name() == hw_id.as_str())
                                    .unwrap_or(false)
                            });
                        if found.is_some() {
                            log::info!("[ALSA Backend] Using fallback device: {}", hw_id);
                        }
                        found
                    }
                    _ => None,
                },
            };

            resolved.ok_or_else(|| {
                let proc_found = is_card_present_in_proc(device_id);
                if proc_found {
                    format!(
                        "Device '{}' is present in /proc/asound but CPAL cannot open it (usually a sample-rate/format mismatch — track rate {}Hz, or an ALSA name format mismatch). Try the plughw plugin in audio settings.",
                        device_id, config.sample_rate
                    )
                } else {
                    format!(
                        "Device '{}' not found by the ALSA backend (disconnected, renamed, or handled by another app)",
                        device_id
                    )
                }
            })?
        } else {
            log::info!("[ALSA Backend] Using default device");
            self.host
                .default_output_device()
                .ok_or("No default ALSA device available")?
        };

        let device_name = device
            .description()
            .map(|desc| desc.name().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        log::info!("[ALSA Backend] Using device: {}", device_name);

        // Check if device supports this configuration
        let supported_configs = device
            .supported_output_configs()
            .map_err(|e| format!("Failed to get supported configs: {}", e))?;

        let mut found_matching = false;
        let mut buffer_bounds = SupportedBufferSize::Unknown;
        for range in supported_configs {
            if range.channels() == config.channels
                && config.sample_rate >= range.min_sample_rate()
                && config.sample_rate <= range.max_sample_rate()
            {
                found_matching = true;
                buffer_bounds = *range.buffer_size();
                log::info!(
                    "[ALSA Backend] Device supports {}Hz (range: {}-{}Hz)",
                    config.sample_rate,
                    range.min_sample_rate(),
                    range.max_sample_rate()
                );
                break;
            }
        }

        // If device doesn't support the requested rate, find best fallback
        let effective_rate = if !found_matching {
            if let Some(rates) = get_supported_sample_rates(&device) {
                let fallback = find_best_fallback_rate(config.sample_rate, &rates);
                log::warn!(
                    "[ALSA Backend] Device doesn't support {}Hz. Supported: {:?}. Falling back to {}Hz (rodio will resample)",
                    config.sample_rate, rates, fallback
                );
                fallback
            } else {
                log::warn!(
                    "[ALSA Backend] Could not determine supported rates, attempting {}Hz anyway",
                    config.sample_rate
                );
                config.sample_rate
            }
        } else {
            config.sample_rate
        };

        // Rebuild StreamConfig with effective rate
        let stream_config = StreamConfig {
            channels: config.channels,
            sample_rate: effective_rate,
            buffer_size: BufferSize::Fixed(playback_period_frames(
                effective_rate, config.exclusive_mode, &buffer_bounds,
            )),
        };

        // Create SupportedStreamConfig
        let supported_config = SupportedStreamConfig::new(
            stream_config.channels,
            stream_config.sample_rate,
            SupportedBufferSize::Range { min: 64, max: 8192 },
            SampleFormat::F32,
        );

        // In exclusive mode, PipeWire may have re-acquired the device after the
        // previous ALSA Direct stream released it. Suspend PipeWire before opening.
        if config.exclusive_mode {
            log::info!(
                "[ALSA Backend] Exclusive mode: suspending PipeWire sinks before CPAL stream"
            );
            suspend_default_sink_for_exclusive();
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        log::info!(
            "[ALSA Backend] Callback period: {:?} at {}Hz (driver bounds {:?})",
            stream_config.buffer_size, effective_rate, buffer_bounds
        );
        // with_supported_config resets buffer_size, so apply the period last.
        let mixer_sink = DeviceSinkBuilder::from_device(device)
            .map_err(|e| {
                if config.exclusive_mode {
                    format!(
                        "Failed to create exclusive ALSA stream at {}Hz: {}. Device may be in use by another application.",
                        effective_rate, e
                    )
                } else {
                    format!("Failed to create ALSA device sink builder at {}Hz: {}", effective_rate, e)
                }
            })?
            .with_supported_config(&supported_config)
            .with_buffer_size(stream_config.buffer_size)
            // #660: shared rate-limited callback + wedge latch (see stream_health).
            .with_error_callback(crate::stream_health::error_callback("ALSA Backend"))
            .open_stream()
            .map_err(|e| {
                if config.exclusive_mode {
                    format!(
                        "Failed to create exclusive ALSA stream at {}Hz: {}. Device may be in use by another application.",
                        effective_rate, e
                    )
                } else {
                    format!("Failed to create ALSA stream at {}Hz: {}", effective_rate, e)
                }
            })?;

        if effective_rate != config.sample_rate {
            log::info!(
                "[ALSA Backend] Output stream created at {}Hz (resampled from {}Hz, exclusive: {})",
                effective_rate,
                config.sample_rate,
                config.exclusive_mode
            );
        } else {
            log::info!(
                "[ALSA Backend] Output stream created successfully at {}Hz (exclusive: {})",
                config.sample_rate,
                config.exclusive_mode
            );
        }

        Ok(mixer_sink)
    }

    fn is_available(&self) -> bool {
        // Check if we can enumerate devices (ALSA is working)
        self.host.output_devices().is_ok()
    }

    fn description(&self) -> &'static str {
        "ALSA Direct - Bit-perfect with optional exclusive hardware access"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_period_retains_time_budget_at_hi_res_and_obeys_device_bounds() {
        let wide = SupportedBufferSize::Range { min: 64, max: 65536 };
        for rate in [44100, 48000, 96000, 192000, 384000] {
            assert_eq!(playback_period_frames(rate, true, &wide) * 20, rate);
            assert_eq!(playback_period_frames(rate, false, &wide) * 10, rate);
        }
        let narrow = SupportedBufferSize::Range { min: 256, max: 4096 };
        assert_eq!(playback_period_frames(192000, true, &narrow), 4096);
        assert_eq!(playback_period_frames(4000, true, &narrow), 256);
        assert_eq!(playback_period_frames(192000, true, &SupportedBufferSize::Unknown), 9600);
    }

    #[test]
    fn build_hw_fallback_id_rewrites_iec958_alias() {
        // The exact case from issue #331 — HifiBerry Digi2 Pro on RPi OS.
        assert_eq!(
            build_hw_fallback_id("iec958:CARD=sndrpihifiberry,DEV=0"),
            Some("hw:CARD=sndrpihifiberry,DEV=0".to_string())
        );
    }

    #[test]
    fn build_hw_fallback_id_handles_every_alias_prefix() {
        assert_eq!(
            build_hw_fallback_id("front:CARD=Generic,DEV=0"),
            Some("hw:CARD=Generic,DEV=0".to_string())
        );
        assert_eq!(
            build_hw_fallback_id("sysdefault:CARD=PCH,DEV=0"),
            Some("hw:CARD=PCH,DEV=0".to_string())
        );
        assert_eq!(
            build_hw_fallback_id("hdmi:CARD=HDMI,DEV=3"),
            Some("hw:CARD=HDMI,DEV=3".to_string())
        );
    }

    #[test]
    fn build_hw_fallback_id_defaults_dev_when_missing() {
        // Some alias forms don't carry DEV=; default to 0 so we still
        // produce a valid hw: id.
        assert_eq!(
            build_hw_fallback_id("iec958:CARD=NameOnly"),
            Some("hw:CARD=NameOnly,DEV=0".to_string())
        );
    }

    #[test]
    fn build_hw_fallback_id_returns_none_for_non_alias_inputs() {
        // Raw hw:/plughw: ids don't need a fallback — they're already
        // the kernel PCM. `default` and unknown shapes don't match any
        // known alias prefix.
        assert_eq!(build_hw_fallback_id("hw:0,0"), None);
        assert_eq!(build_hw_fallback_id("plughw:0,0"), None);
        assert_eq!(build_hw_fallback_id("default"), None);
        assert_eq!(build_hw_fallback_id("pulse"), None);
        assert_eq!(build_hw_fallback_id(""), None);
    }

    #[test]
    fn extract_card_name_from_device_handles_alias_prefixes() {
        // Alias forms — pure string parse, no /proc lookup involved.
        assert_eq!(
            extract_card_name_from_device("iec958:CARD=sndrpihifiberry,DEV=0"),
            Some("sndrpihifiberry".to_string())
        );
        assert_eq!(
            extract_card_name_from_device("front:CARD=Generic,DEV=0"),
            Some("Generic".to_string())
        );
        assert_eq!(
            extract_card_name_from_device("hdmi:CARD=HDMI_C,DEV=3"),
            Some("HDMI_C".to_string())
        );
        assert_eq!(
            extract_card_name_from_device("sysdefault:CARD=PCH,DEV=0"),
            Some("PCH".to_string())
        );
    }

    #[test]
    fn extract_card_name_from_device_rejects_non_card_pcms() {
        // These shapes don't carry a CARD= component and should not
        // resolve to anything in /proc/asound.
        assert_eq!(extract_card_name_from_device("default"), None);
        assert_eq!(extract_card_name_from_device("pulse"), None);
        assert_eq!(extract_card_name_from_device("null"), None);
        assert_eq!(extract_card_name_from_device(""), None);
    }

    #[test]
    fn is_card_present_in_proc_short_circuits_on_unparseable_ids() {
        // For inputs without a CARD= component, the helper must short-
        // circuit to false without ever touching /proc — so this stays
        // safe regardless of host audio configuration.
        assert!(!is_card_present_in_proc("default"));
        assert!(!is_card_present_in_proc("pulse"));
        assert!(!is_card_present_in_proc("null"));
        assert!(!is_card_present_in_proc(""));
    }

    #[test]
    fn is_known_pcm_id_keeps_only_lookup_targets() {
        // Positive: every shape downstream code actually queries.
        assert!(is_known_pcm_id("default"));
        assert!(is_known_pcm_id("sysdefault:CARD=PCH"));
        assert!(is_known_pcm_id("front:CARD=Generic,DEV=0"));
        assert!(is_known_pcm_id("hdmi:CARD=HDMI,DEV=3"));
        assert!(is_known_pcm_id("iec958:CARD=sndrpihifiberry,DEV=0"));

        // Negative: virtual PCMs that only emit noise when probed.
        assert!(!is_known_pcm_id("dmix:CARD=PCH,DEV=0"));
        assert!(!is_known_pcm_id("dsnoop:CARD=PCH,DEV=0"));
        assert!(!is_known_pcm_id("route:CARD=PCH"));
        assert!(!is_known_pcm_id("surround51:CARD=PCH"));
        assert!(!is_known_pcm_id("pulse"));
        assert!(!is_known_pcm_id("null"));
        assert!(!is_known_pcm_id("hw:0,0"));
        assert!(!is_known_pcm_id("plughw:0,0"));
    }

    #[test]
    fn raw_open_ids_derives_raw_pair_from_front_alias() {
        // Discussion #641 — snd-aloop declares `front` for DEV=0 only, so a
        // saved `front:CARD=Loopback,DEV=1` fails to open with ENOENT. The
        // derived raw ids take CARD=/DEV= straight to the kernel PCM.
        assert_eq!(
            raw_open_ids("front:CARD=Loopback,DEV=1"),
            Some((
                "hw:CARD=Loopback,DEV=1".to_string(),
                "plughw:CARD=Loopback,DEV=1".to_string()
            ))
        );
        assert_eq!(
            raw_open_ids("front:CARD=ZH3,DEV=0"),
            Some((
                "hw:CARD=ZH3,DEV=0".to_string(),
                "plughw:CARD=ZH3,DEV=0".to_string()
            ))
        );
    }

    #[test]
    fn raw_open_ids_defaults_dev_when_missing() {
        assert_eq!(
            raw_open_ids("front:CARD=Loopback"),
            Some((
                "hw:CARD=Loopback,DEV=0".to_string(),
                "plughw:CARD=Loopback,DEV=0".to_string()
            ))
        );
    }

    #[test]
    fn raw_open_ids_passes_through_non_alias_ids() {
        // Raw and virtual ids keep the caller's pre-existing handling.
        assert_eq!(raw_open_ids("hw:0,0"), None);
        assert_eq!(raw_open_ids("plughw:1,0"), None);
        assert_eq!(raw_open_ids("default"), None);
        assert_eq!(raw_open_ids("pulse"), None);
    }
}
