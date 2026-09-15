//! Direct ALSA access using alsa-rs
//!
//! Provides bit-perfect playback for hw:X,Y devices that CPAL cannot open.
//! This module bypasses rodio/CPAL completely for direct hardware access.

#[cfg(target_os = "linux")]
use alsa::pcm::{Access, Format, Frames, HwParams, PCM};
#[cfg(target_os = "linux")]
use alsa::{Direction, ValueOr};
#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicBool;
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};

/// Log a PCM recovery and record it as a network-throttle underrun signal.
///
/// Each call to ALSA's `pcm.recover()` that returns successfully indicates
/// that the writer thread fell behind the kernel's playback buffer — i.e.
/// an audio underrun. The network throttle treats this as the strongest
/// possible "slow down" signal and immediately drops the prefetch cap to
/// zero for `PANIC_WINDOW_SECS`, so the live stream gets the full pipe to
/// recover.
#[cfg(target_os = "linux")]
fn log_pcm_recovery(suffix: &str) {
    if suffix.is_empty() {
        log::warn!("[ALSA Direct] Recovered from PCM error");
    } else {
        log::warn!("[ALSA Direct] Recovered from PCM error {}", suffix);
    }
    crate::network_throttle::state().record_underrun();
}

/// Recover a failed write. `snd_pcm_recover` handles EPIPE/ESTRPIPE but NOT
/// EBADFD on this stack (observed on the Pi: recover itself returns 77) — and
/// EBADFD is what a write gets when it races a still-DRAINING pcm (natural-end
/// drain + a late append of the next track) or a stream left in limbo by a
/// failed prepare. For EBADFD, cancel the drain (drop) and prepare explicitly:
/// the stream is writable again and the NEW track's following chunks flow (the
/// one rejected chunk, ~50 ms, is lost — the drain was about to be cut anyway).
/// `snd_pcm_recover` still gets first try: on stacks where it DOES handle
/// EBADFD this behaves exactly as before.
#[cfg(target_os = "linux")]
fn recover_write_error(pcm: &PCM, errno: i32, suffix: &str) -> Result<(), String> {
    // libc::EBADFD — pcm not in a writable state (e.g. mid-drain).
    const EBADFD: i32 = 77;
    match pcm.recover(errno, false) {
        Ok(()) => {
            log_pcm_recovery(suffix);
            Ok(())
        }
        Err(recover_err) if errno == EBADFD => {
            log::warn!(
                "[ALSA Direct] recover(EBADFD) unsupported ({recover_err}); drop+prepare to cancel the drain"
            );
            // UFCS: `pcm.drop()` resolves to `Drop::drop` — name the inherent
            // method explicitly (same gotcha as in `stop()`).
            PCM::drop(pcm).map_err(|e| format!("drop after EBADFD failed: {e}"))?;
            pcm.prepare()
                .map_err(|e| format!("prepare after EBADFD failed: {e}"))?;
            log_pcm_recovery(suffix);
            Ok(())
        }
        Err(recover_err) => Err(format!("Failed to recover from error: {recover_err}")),
    }
}

/// Fail closed when ALSA selected a different rate than requested (exclusive /
/// bit-perfect paths must not silently nearest-neighbor).
#[cfg(target_os = "linux")]
fn ensure_exact_rate(hwp: &HwParams<'_>, requested: u32, kind: &str) -> Result<(), String> {
    let actual = hwp
        .get_rate()
        .map_err(|e| format!("Failed to read back {kind} sample rate: {e}"))?;
    if actual != requested {
        return Err(format!(
            "ALSA {kind} rate mismatch: requested {requested} Hz, device selected {actual} Hz (refusing non-bit-perfect nearest)"
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_pcm_frames(
    pcm: &PCM, frames: usize, cancel: &AtomicBool,
    mut write: impl FnMut(usize) -> Result<usize, alsa::Error>,
) -> Result<usize, crate::backend::DirectWriteError> {
    crate::pcm_write::write_complete(frames, cancel, std::time::Duration::from_secs(2),
        |offset| write(offset).map_err(|e| e.errno()),
        |error| match error {
            libc::EPIPE => { pcm.prepare().map_err(|e| e.to_string())?; log_pcm_recovery(""); Ok(true) }
            libc::ESTRPIPE => match pcm.resume() {
                Ok(()) => Ok(true),
                Err(e) if e.errno() == libc::EAGAIN => Ok(false),
                Err(_) => { pcm.prepare().map_err(|e| e.to_string())?; Ok(true) }
            },
            libc::EBADFD => {
                let _ = PCM::drop(pcm);
                pcm.prepare().map_err(|e| e.to_string())?;
                Ok(true)
            }
            _ => Err(format!("ALSA write failed (errno {error})")),
        },
        || match pcm.wait(Some(25)) {
            Ok(_) => Ok(()),
            Err(e) if matches!(e.errno(), libc::EINTR | libc::EPIPE | libc::ESTRPIPE) => Ok(()),
            Err(e) => Err(format!("ALSA wait failed: {e}")),
        })
}

/// Direct ALSA PCM stream for hw: devices
///
/// Field order is significant: Rust drops struct fields top-to-bottom, so the
/// `PCM` is dropped first (releasing the kernel-level exclusive grip on the
/// `hw:` device) BEFORE `_reservation` drops (releasing the
/// `org.freedesktop.ReserveDevice1.Audio<N>` bus name back to PipeWire).
///
/// Reversing this order would tell PipeWire "go ahead, take the device" while
/// the kernel still has the FD open — guaranteed `EBUSY` ping-pong on the next
/// stream open. `_reservation` is intentionally the last field for that
/// reason; do not rearrange.
#[cfg(target_os = "linux")]
pub struct AlsaDirectStream {
    pcm: Arc<Mutex<PCM>>,
    #[allow(dead_code)]
    is_playing: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u16,
    format: Format,
    device_id: String,
    /// D-Bus device reservation held for the entire stream lifetime
    /// (Lifetime A per the design spec). Acquired before `PCM::new()` in
    /// `Self::new()`; released on `Drop` *after* the PCM closes (see field-order
    /// note on the struct above).
    _reservation: crate::DeviceReservation,
}

#[cfg(not(target_os = "linux"))]
pub struct AlsaDirectStream {
    sample_rate: u32,
    channels: u16,
    device_id: String,
}

/// A writable ALSA mixer element and its current normalized level.
///
/// The probe is deliberately read-only. Callers use the sampled level to seed
/// QBZ's volume state *before* enabling hardware volume, so changing the
/// setting cannot copy the direct path's fixed 100% UI value into the
/// physical mixer.
#[derive(Clone, Debug, PartialEq)]
pub struct HardwareVolumeInfo {
    pub control_name: String,
    pub volume: f32,
}

/// Defensive settle delay between reservation acquisition and PCM open.
///
/// Only applied when the reservation actually transitioned ownership (i.e.
/// `DeviceReservation::is_active()` is `true`). Sized conservatively; do not
/// reduce without revisiting the Lifetime-A safety contract in
/// `qbz-nix-docs/specs/2026-05-07-alsa-exclusive-hardening-design.md`.
#[cfg(target_os = "linux")]
const PIPEWIRE_VACATE_MARGIN: std::time::Duration = std::time::Duration::from_millis(50);

#[cfg(target_os = "linux")]
impl AlsaDirectStream {
    /// Create new ALSA direct stream
    pub fn new(device_id: &str, sample_rate: u32, channels: u16) -> Result<Self, String> {
        log::info!(
            "[ALSA Direct] Opening device: {} ({}Hz, {}ch)",
            device_id,
            sample_rate,
            channels
        );

        // Acquire D-Bus device reservation BEFORE opening the PCM. This signals
        // PipeWire/WirePlumber to release the device first if it currently
        // holds it. Held for the entire `AlsaDirectStream` lifetime
        // (Lifetime A per the design spec) and released on `Drop` after the
        // PCM closes — see the field-order comment on the struct.
        //
        // This is the canonical Lifetime-A consumer the `acquire` doc-comment's
        // tight-coupling rule allows: a `DeviceReservation` is created
        // immediately before a real `PCM::new()` and held for as long as that
        // PCM is open.
        // TODO(Task 5): replace second arg with user-facing DAC name from settings.
        let reservation = crate::DeviceReservation::acquire(device_id, device_id)
            .map_err(|e| format!("Cannot acquire exclusive device '{}': {}", device_id, e))?;

        // Defensive margin only matters when the reservation actually displaced
        // a holder (or could have). On the degraded D-Bus path the bus name is
        // not held at all, so PipeWire's view of the device hasn't changed and
        // no settle delay is needed. PIPEWIRE_VACATE_MARGIN is conservative;
        // PipeWire-side release latency is typically much shorter, but this
        // margin is part of the design spec's Lifetime-A safety contract — do
        // not reduce without revisiting the spec.
        if reservation.is_active() {
            std::thread::sleep(PIPEWIRE_VACATE_MARGIN);
        }

        // Open PCM device
        let pcm = PCM::new(device_id, Direction::Playback, true)
            .map_err(|e| format!("Failed to open ALSA device '{}': {}", device_id, e))?;

        // Set hardware parameters and auto-detect best format
        let selected_format = {
            let hwp =
                HwParams::any(&pcm).map_err(|e| format!("Failed to get hardware params: {}", e))?;

            // Set access type (interleaved)
            hwp.set_access(Access::RWInterleaved)
                .map_err(|e| format!("Failed to set access: {}", e))?;

            // Try formats in order of preference for bit-perfect playback
            // S24_3LE first: required by SMSL-class USB DACs (TAS1020B chip)
            // Then descending bit-depth for quality
            let format_priority = [
                (Format::S243LE, "S24_3LE"), // 24-bit packed (SMSL, Topping, Fosi DACs)
                (Format::S32LE, "S32LE"),    // 32-bit
                (Format::S24LE, "S24LE"),    // 24-bit in 32-bit container
                (Format::S16LE, "S16LE"),    // 16-bit
                (Format::FloatLE, "Float32LE"), // Float (compatibility)
            ];

            let mut selected_format = None;
            for (format, name) in &format_priority {
                if hwp.set_format(*format).is_ok() {
                    log::info!("[ALSA Direct] Selected format: {}", name);
                    selected_format = Some(*format);
                    break;
                }
            }

            let format = selected_format.ok_or_else(|| {
                "No supported audio format found (tried S24_3LE, S32LE, S24LE, S16LE, FloatLE)"
                    .to_string()
            })?;

            // Set channels
            hwp.set_channels(channels as u32)
                .map_err(|e| format!("Failed to set channels: {}", e))?;

            // Request the track rate. ValueOr::Nearest is still used so ALSA
            // accepts the set; we fail closed below if hardware did not match.
            hwp.set_rate(sample_rate, ValueOr::Nearest)
                .map_err(|e| format!("Failed to set sample rate: {}", e))?;

            // Set buffer size (larger buffer for high-res audio)
            let buffer_size = if sample_rate >= 192000 {
                // 500ms buffer for 192kHz+ (like MPD config)
                (sample_rate / 2) as Frames
            } else if sample_rate >= 96000 {
                // 250ms buffer for 96kHz
                (sample_rate / 4) as Frames
            } else {
                // 125ms buffer for lower rates
                (sample_rate / 8) as Frames
            };

            hwp.set_buffer_size_near(buffer_size)
                .map_err(|e| format!("Failed to set buffer size: {}", e))?;

            // Set period size (1/10 of buffer)
            hwp.set_period_size_near(buffer_size / 10, ValueOr::Nearest)
                .map_err(|e| format!("Failed to set period size: {}", e))?;

            // Apply hardware parameters
            pcm.hw_params(&hwp)
                .map_err(|e| format!("Failed to apply hardware params: {}", e))?;

            ensure_exact_rate(&hwp, sample_rate, "exclusive PCM")?;

            log::info!(
                "[ALSA Direct] Hardware configured: {}Hz, {}ch, buffer: {} frames, format: {:?}",
                sample_rate,
                channels,
                buffer_size,
                format
            );

            format
        };

        // Prepare device for playback
        pcm.prepare()
            .map_err(|e| format!("Failed to prepare PCM: {}", e))?;

        Ok(Self {
            pcm: Arc::new(Mutex::new(pcm)),
            is_playing: Arc::new(AtomicBool::new(false)),
            sample_rate,
            channels,
            format: selected_format,
            device_id: device_id.to_string(),
            // Last field: drops after `pcm` so the kernel-level exclusive
            // grip is released before the D-Bus bus name is freed.
            _reservation: reservation,
        })
    }

    /// Create an ALSA direct stream for DoP (DSD over PCM) delivery.
    ///
    /// ADDITIVE to the protected PCM paths (DSD plan Phase 2, owner-approved
    /// 2026-07-03): S32_LE ONLY — DoP words are pre-packed 24-bit frames
    /// left-justified in S32 and must reach the device bit-exactly, so no
    /// format fallback, no plughw, no float. If the device has no S32_LE at
    /// the carrier rate the caller falls back to DSD→PCM conversion.
    /// Mirrors `new()` for reservation / buffer sizing / field order.
    pub fn new_dop(device_id: &str, carrier_rate: u32, channels: u16) -> Result<Self, String> {
        log::info!(
            "[ALSA Direct] Opening device for DoP: {} ({}Hz carrier, {}ch, S32_LE)",
            device_id,
            carrier_rate,
            channels
        );

        let reservation = crate::DeviceReservation::acquire(device_id, device_id)
            .map_err(|e| format!("Cannot acquire exclusive device '{}': {}", device_id, e))?;
        if reservation.is_active() {
            std::thread::sleep(PIPEWIRE_VACATE_MARGIN);
        }

        let pcm = PCM::new(device_id, Direction::Playback, false)
            .map_err(|e| format!("Failed to open ALSA device '{}': {}", device_id, e))?;

        {
            let hwp =
                HwParams::any(&pcm).map_err(|e| format!("Failed to get hardware params: {}", e))?;
            hwp.set_access(Access::RWInterleaved)
                .map_err(|e| format!("Failed to set access: {}", e))?;
            hwp.set_format(Format::S32LE)
                .map_err(|e| format!("Device has no S32_LE (required for DoP): {}", e))?;
            hwp.set_channels(channels as u32)
                .map_err(|e| format!("Failed to set channels: {}", e))?;
            hwp.set_rate(carrier_rate, ValueOr::Nearest)
                .map_err(|e| format!("Failed to set DoP carrier rate {}: {}", carrier_rate, e))?;
            let buffer_size = if carrier_rate >= 192000 {
                (carrier_rate / 2) as Frames
            } else if carrier_rate >= 96000 {
                (carrier_rate / 4) as Frames
            } else {
                (carrier_rate / 8) as Frames
            };
            hwp.set_buffer_size_near(buffer_size)
                .map_err(|e| format!("Failed to set buffer size: {}", e))?;
            hwp.set_period_size_near(buffer_size / 10, ValueOr::Nearest)
                .map_err(|e| format!("Failed to set period size: {}", e))?;
            pcm.hw_params(&hwp)
                .map_err(|e| format!("Failed to apply hardware params: {}", e))?;
            ensure_exact_rate(&hwp, carrier_rate, "DoP carrier")?;
            log::info!(
                "[ALSA Direct] DoP hardware configured: {}Hz, {}ch, S32_LE, buffer {} frames",
                carrier_rate,
                channels,
                buffer_size
            );
        }

        pcm.prepare()
            .map_err(|e| format!("Failed to prepare PCM: {}", e))?;

        Ok(Self {
            pcm: Arc::new(Mutex::new(pcm)),
            is_playing: Arc::new(AtomicBool::new(false)),
            sample_rate: carrier_rate,
            channels,
            format: Format::S32LE,
            device_id: device_id.to_string(),
            // Last field: drops after `pcm` (see field-order note on the struct).
            _reservation: reservation,
        })
    }

    /// Create an ALSA direct stream for NATIVE DSD (DSD plan Phase 3).
    ///
    /// ADDITIVE like `new_dop`. Tries `DSD_U32_BE` first (what the kernel's
    /// generic USB DSD quirk grants), then `DSD_U32_LE`. Frame rate =
    /// dsd_rate / 32. Returns the stream plus `little_endian` so the packer
    /// lays the 4 DSD bytes out correctly. Fails cleanly when the kernel
    /// hasn't granted the device a DSD format (no quirk) — the caller falls
    /// back to DoP/conversion.
    pub fn new_native_dsd(
        device_id: &str,
        dsd_rate: u32,
        channels: u16,
    ) -> Result<(Self, bool), String> {
        let rate = dsd_rate / 32;
        log::info!(
            "[ALSA Direct] Opening device for native DSD: {} ({} DSD bits/s → {} Hz U32, {}ch)",
            device_id,
            dsd_rate,
            rate,
            channels
        );

        let reservation = crate::DeviceReservation::acquire(device_id, device_id)
            .map_err(|e| format!("Cannot acquire exclusive device '{}': {}", device_id, e))?;
        if reservation.is_active() {
            std::thread::sleep(PIPEWIRE_VACATE_MARGIN);
        }

        let pcm = PCM::new(device_id, Direction::Playback, false)
            .map_err(|e| format!("Failed to open ALSA device '{}': {}", device_id, e))?;

        let selected = {
            let hwp =
                HwParams::any(&pcm).map_err(|e| format!("Failed to get hardware params: {}", e))?;
            hwp.set_access(Access::RWInterleaved)
                .map_err(|e| format!("Failed to set access: {}", e))?;
            let mut selected = None;
            for (format, le) in [(Format::DSDU32BE, false), (Format::DSDU32LE, true)] {
                if hwp.set_format(format).is_ok() {
                    selected = Some((format, le));
                    break;
                }
            }
            let Some((format, le)) = selected else {
                return Err("Device has no native DSD format (kernel quirk missing?)".to_string());
            };
            hwp.set_channels(channels as u32)
                .map_err(|e| format!("Failed to set channels: {}", e))?;
            hwp.set_rate(rate, ValueOr::Nearest)
                .map_err(|e| format!("Failed to set native DSD rate {}: {}", rate, e))?;
            let buffer_size = (rate / 4) as Frames; // 250 ms
            hwp.set_buffer_size_near(buffer_size)
                .map_err(|e| format!("Failed to set buffer size: {}", e))?;
            hwp.set_period_size_near(buffer_size / 10, ValueOr::Nearest)
                .map_err(|e| format!("Failed to set period size: {}", e))?;
            pcm.hw_params(&hwp)
                .map_err(|e| format!("Failed to apply hardware params: {}", e))?;
            ensure_exact_rate(&hwp, rate, "native DSD")?;
            log::info!(
                "[ALSA Direct] Native DSD configured: {:?} @ {} Hz, {}ch",
                format,
                rate,
                channels
            );
            (format, le)
        };

        pcm.prepare()
            .map_err(|e| format!("Failed to prepare PCM: {}", e))?;

        Ok((
            Self {
                pcm: Arc::new(Mutex::new(pcm)),
                is_playing: Arc::new(AtomicBool::new(false)),
                sample_rate: rate,
                channels,
                format: selected.0,
                device_id: device_id.to_string(),
                // Last field: drops after `pcm` (see field-order note).
                _reservation: reservation,
            },
            selected.1,
        ))
    }

    /// Write pre-packed 32-bit direct words (DoP frames in S32, or native
    /// DSD_U32 words) VERBATIM — no scaling, no conversion. Only valid on
    /// streams created with [`Self::new_dop`] / [`Self::new_native_dsd`].
    /// DSD_U32 formats fail alsa-rs's checked-format i32 IO, so they use the
    /// unchecked accessor — sound because both layouts are exactly 32 bits
    /// per channel per frame, same as S32.
    pub fn write_dop_i32(&self, samples: &[i32]) -> Result<(), String> {
        let pcm = self.pcm.lock().unwrap();
        let frames = samples.len() / self.channels as usize;
        let io = if self.format == Format::S32LE {
            pcm.io_i32()
                .map_err(|e| format!("Failed to get PCM I/O: {}", e))?
        } else {
            unsafe { pcm.io_unchecked::<i32>() }
        };
        match io.writei(samples) {
            Ok(written) => {
                if written != frames {
                    log::warn!(
                        "[ALSA Direct] Partial DoP write: {} / {} frames",
                        written,
                        frames
                    );
                }
                Ok(())
            }
            Err(e) => {
                if let Err(msg) = recover_write_error(&pcm, e.errno() as i32, "(DoP)") {
                    Err(msg)
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Write audio samples to ALSA (auto-converts from i16 based on detected format)
    pub fn write(&self, samples_i16: &[i16]) -> Result<(), String> {
        let pcm = self.pcm.lock().unwrap();
        if self.channels == 0 || samples_i16.len() % usize::from(self.channels) != 0 {
            return Err("PCM input does not contain complete frames".into());
        }
        let frames = samples_i16.len() / self.channels as usize;
        let cancel = &AtomicBool::new(false);

        match self.format {
            Format::FloatLE => {
                // Convert i16 to f32
                let samples_f32: Vec<f32> =
                    samples_i16.iter().map(|&s| s as f32 / 32768.0).collect();

                let io = pcm
                    .io_f32()
                    .map_err(|e| format!("Failed to get PCM I/O: {}", e))?;

                write_pcm_frames(&pcm, frames, cancel, |offset| {
                    io.writei(&samples_f32[offset * (usize::from(self.channels))..])
                }).map(|_| ()).map_err(|e| e.message)
            }
            Format::S32LE => {
                // Convert i16 to i32 (bit-perfect: shift left 16 bits)
                let samples_i32: Vec<i32> = samples_i16.iter().map(|&s| (s as i32) << 16).collect();

                let io = pcm
                    .io_i32()
                    .map_err(|e| format!("Failed to get PCM I/O: {}", e))?;

                write_pcm_frames(&pcm, frames, cancel, |offset| {
                    io.writei(&samples_i32[offset * (usize::from(self.channels))..])
                }).map(|_| ()).map_err(|e| e.message)
            }
            Format::S16LE => {
                // Direct write (no conversion needed)
                let io = pcm
                    .io_i16()
                    .map_err(|e| format!("Failed to get PCM I/O: {}", e))?;

                write_pcm_frames(&pcm, frames, cancel, |offset| {
                    io.writei(&samples_i16[offset * (usize::from(self.channels))..])
                }).map(|_| ()).map_err(|e| e.message)
            }
            Format::S243LE => {
                // S24_3LE: 24-bit packed in 3 bytes, little-endian
                // Required by SMSL-class USB DACs (TAS1020B chip)
                // Convert i16 → i24: shift left 8 bits, then pack into 3 bytes
                let mut bytes: Vec<u8> = Vec::with_capacity(samples_i16.len() * 3);

                for &sample in samples_i16 {
                    // Convert i16 to i24 (lossless: zeros in lower 8 bits)
                    let s24 = (sample as i32) << 8;
                    // Pack as 3 bytes in little-endian order
                    bytes.push((s24 & 0xFF) as u8); // LSB
                    bytes.push(((s24 >> 8) & 0xFF) as u8); // Middle
                    bytes.push(((s24 >> 16) & 0xFF) as u8); // MSB (sign-extended)
                }

                // Use raw byte I/O for 3-byte packed format
                let io = pcm.io_bytes();

                write_pcm_frames(&pcm, frames, cancel, |offset| {
                    io.writei(&bytes[offset * (usize::from(self.channels) * 3)..])
                }).map(|_| ()).map_err(|e| e.message)
            }
            Format::S24LE => {
                // S24LE: 24-bit in 32-bit container (padded)
                // Convert i16 → i32, shift left 16 bits (same as S32LE for i16 source)
                let samples_i32: Vec<i32> = samples_i16.iter().map(|&s| (s as i32) << 16).collect();

                let io = pcm
                    .io_i32()
                    .map_err(|e| format!("Failed to get PCM I/O: {}", e))?;

                write_pcm_frames(&pcm, frames, cancel, |offset| {
                    io.writei(&samples_i32[offset * (usize::from(self.channels))..])
                }).map(|_| ()).map_err(|e| e.message)
            }
            _ => Err(format!("Unsupported format: {:?}", self.format)),
        }
    }

    /// Write f32 audio samples to ALSA (converts to hardware format with full precision)
    ///
    /// f32 has 24 bits of significand, so 24-bit audio is preserved losslessly.
    /// This is the primary write path for the f32 pipeline.
    pub fn write_f32(&self, samples_f32: &[f32]) -> Result<(), String> {
        self.write_f32_with_cancel(samples_f32, &AtomicBool::new(false)).map(|_| ()).map_err(|e| e.message)
    }

    fn write_f32_with_cancel(&self, samples_f32: &[f32], cancel: &AtomicBool)
        -> Result<usize, crate::backend::DirectWriteError> {
        if self.channels == 0 || samples_f32.len() % usize::from(self.channels) != 0 {
            return Err("PCM input does not contain complete frames".to_string().into());
        }
        let pcm = self.pcm.lock().unwrap();
        let frames = samples_f32.len() / self.channels as usize;

        match self.format {
            Format::FloatLE => {
                // Direct write - no conversion needed
                let io = pcm
                    .io_f32()
                    .map_err(|e| format!("Failed to get PCM I/O: {}", e))?;

                write_pcm_frames(&pcm, frames, cancel, |offset| {
                    io.writei(&samples_f32[offset * (usize::from(self.channels))..])
                })
            }
            Format::S32LE => {
                // f32 [-1.0, 1.0] -> i32 full range
                let samples_i32: Vec<i32> = samples_f32
                    .iter()
                    .map(|&s| (s * 2_147_483_647.0) as i32)
                    .collect();

                let io = pcm
                    .io_i32()
                    .map_err(|e| format!("Failed to get PCM I/O: {}", e))?;

                write_pcm_frames(&pcm, frames, cancel, |offset| {
                    io.writei(&samples_i32[offset * (usize::from(self.channels))..])
                })
            }
            Format::S24LE => {
                // f32 -> 24-bit in 32-bit container
                // Clamp to 24-bit range: [-8388608, 8388607]
                let samples_i32: Vec<i32> = samples_f32
                    .iter()
                    .map(|&s| crate::pcm_sample::s24(s))
                    .collect();

                let io = pcm
                    .io_i32()
                    .map_err(|e| format!("Failed to get PCM I/O: {}", e))?;

                write_pcm_frames(&pcm, frames, cancel, |offset| {
                    io.writei(&samples_i32[offset * (usize::from(self.channels))..])
                })
            }
            Format::S243LE => {
                // S24_3LE: 24-bit packed in 3 bytes, little-endian
                // f32 -> 24-bit integer, packed into 3 bytes
                let mut bytes: Vec<u8> = Vec::with_capacity(samples_f32.len() * 3);

                for &sample in samples_f32 {
                    bytes.extend_from_slice(&crate::pcm_sample::s24_packed(sample));
                }

                let io = pcm.io_bytes();

                write_pcm_frames(&pcm, frames, cancel, |offset| {
                    io.writei(&bytes[offset * (usize::from(self.channels) * 3)..])
                })
            }
            Format::S16LE => {
                // f32 -> i16
                let samples_i16: Vec<i16> =
                    samples_f32.iter().map(|&s| crate::pcm_sample::s16(s)).collect();

                let io = pcm
                    .io_i16()
                    .map_err(|e| format!("Failed to get PCM I/O: {}", e))?;

                write_pcm_frames(&pcm, frames, cancel, |offset| {
                    io.writei(&samples_i16[offset * (usize::from(self.channels))..])
                })
            }
            _ => Err(format!("Unsupported format: {:?}", self.format).into()),
        }
    }

    /// Drain and stop playback
    pub fn drain(&self) -> Result<(), String> {
        self.drain_with_cancel(&AtomicBool::new(false))
    }

    fn drain_with_cancel(&self, cancel: &AtomicBool) -> Result<(), String> {
        log::info!("[ALSA Direct] Draining PCM");
        let pcm = self.pcm.lock().unwrap();
        // BOUNDED drain — a bare `snd_pcm_drain` blocks until every queued
        // frame clocks out, and on this driver (snd-rpi-hifiberry/PCM5122) it
        // can block FOREVER when the device stops clocking (observed on the
        // Pi: natural track end -> drain never returned -> the writer thread
        // wedged -> no "engine empty" -> playback died at the transition,
        // "dos tracks y pausa"). Poll the state instead: while frames clock
        // out the pcm is Running; when the tail finishes it underruns to XRun
        // (no more writes are coming) — that IS the drained end state, so
        // drop+prepare and return. If it neither drains nor underruns within
        // the deadline, cancel with drop+prepare so the transition survives.
        const DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
        let start = std::time::Instant::now();
        loop {
            if cancel.load(std::sync::atomic::Ordering::Acquire) { return Ok(()); }
            match pcm.state() {
                alsa::pcm::State::Running | alsa::pcm::State::Draining => {
                    if start.elapsed() >= DRAIN_DEADLINE {
                        log::warn!(
                            "[ALSA Direct] drain deadline (10s) hit — drop+prepare to unstick the pcm"
                        );
                        // UFCS: `pcm.drop()` resolves to `Drop::drop` on the
                        // MutexGuard — name the inherent method explicitly.
                        PCM::drop(&pcm).map_err(|e| format!("drop after stuck drain: {e}"))?;
                        return pcm
                            .prepare()
                            .map_err(|e| format!("prepare after stuck drain: {e}"));
                    }
                    // Sleep in 100 ms slices waiting for the device to clock.
                    let _ = pcm.wait(Some(100));
                }
                alsa::pcm::State::XRun => {
                    // Tail finished (natural underrun at end-of-stream) or the
                    // frames are gone either way — reset for the next track.
                    PCM::drop(&pcm).map_err(|e| format!("drop after drain XRUN: {e}"))?;
                    return pcm
                        .prepare()
                        .map_err(|e| format!("prepare after drain XRUN: {e}"));
                }
                // Already drained (Setup/Prepared), Paused, or anything else:
                // nothing to wait for.
                _ => return Ok(()),
            }
        }
    }

    /// Stop PCM immediately (prepare for next playback)
    pub fn stop(&self) -> Result<(), String> {
        log::info!("[ALSA Direct] Stopping PCM");
        let pcm = self.pcm.lock().unwrap();
        // Standard immediate-stop ritual: DROP (halt now, discard queued
        // frames) THEN prepare. prepare() alone on a RUNNING or DRAINING
        // stream fails with EBUSY on drivers that require an explicit drop
        // first (snd-rpi-hifiberry / PCM5122 — every stop on the Pi logged
        // "Device or resource busy (16)"), and each failed prepare left the
        // PCM in a limbo the NEXT stream's write surfaced as unrecoverable
        // EBADFD, killing the track transition. drop() from a non-running
        // state returns EBADFD — nothing was playing; harmless, ignore.
        // UFCS: `pcm.drop()` would resolve to `Drop::drop` on the MutexGuard
        // (the guard is the first deref step with a `drop` candidate) — the
        // inherent PCM method must be named explicitly.
        if let Err(e) = PCM::drop(&pcm) {
            // libc::EBADFD — the PCM was not in a running-ish state.
            const EBADFD: i32 = 77;
            if e.errno() as i32 != EBADFD {
                log::warn!(
                    "[ALSA Direct] drop on stop failed (continuing to prepare): {}",
                    e
                );
            }
        }
        pcm.prepare()
            .map_err(|e| format!("Failed to prepare PCM after stop: {}", e))
    }

    /// Get sample rate
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Get channels
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Frames accepted by ALSA but not yet presented by the device.
    ///
    /// This is observational only: it neither changes PCM parameters nor
    /// waits for the device. The direct writer uses it to point the passive
    /// visualizer tap at the audible playhead instead of the queue head.
    pub fn playback_delay_frames(&self) -> Result<u64, String> {
        let pcm = self.pcm.lock().unwrap();
        pcm.delay()
            .map(|frames| frames.max(0) as u64)
            .map_err(|e| format!("Failed to query ALSA playback delay: {}", e))
    }

    /// Get device ID
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Try to set hardware volume via ALSA mixer.
    ///
    /// Returns error if:
    /// - DAC doesn't have mixer controls (common for USB DACs)
    /// - Mixer API fails
    ///
    /// NOTE: Failure doesn't break playback, just means volume can't be controlled.
    pub fn set_hardware_volume(
        &self,
        control: &crate::alsa_hardware_volume::AlsaMixerControlId,
        volume: f32,
    ) -> Result<(), String> {
        crate::alsa_hardware_volume::set_hardware_volume(&self.device_id, control, volume)
            .map(|_| ())
    }

    /// Check if device is a bit-perfect hardware device
    /// Includes: hw:X,Y, plughw:X,Y, and front:CARD=X,DEV=Y
    pub fn is_hw_device(device_id: &str) -> bool {
        device_id.starts_with("hw:")
            || device_id.starts_with("plughw:")
            || device_id.starts_with("front:CARD=")
    }
}

/// Read-only capability probe used before the UI enables hardware volume.
pub fn probe_hardware_volume(device_id: &str) -> Result<HardwareVolumeInfo, String> {
    use crate::alsa_hardware_volume::{
        resolve_hardware_volume, HardwareVolumeDecision, HardwareVolumeUnsupportedReason,
    };
    let decision = resolve_hardware_volume(device_id, &Default::default());
    match decision {
        HardwareVolumeDecision::Selected { selection } => Ok(HardwareVolumeInfo {
            control_name: selection.control.name,
            volume: selection.candidate.current_volume.unwrap_or(1.0),
        }),
        HardwareVolumeDecision::NeedsChoice { candidates, .. } => Err(format!(
            "Ambiguous playback-volume controls for {device_id}: {}",
            candidates
                .iter()
                .map(|candidate| candidate.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        HardwareVolumeDecision::Unsupported { reason, .. } => match reason {
            HardwareVolumeUnsupportedReason::Probe(error) => Err(error.message),
            other => Err(format!(
                "No compatible ALSA hardware volume for {device_id}: {other:?}"
            )),
        },
    }
}

/// Mirrors the exact route predicate used by the player: ALSA plus a direct
/// device id and either the `hw` or `plughw` engine. ALSA's default/sysdefault
/// and `Pcm` routes use CPAL/Rodio and retain software volume.
pub fn uses_alsa_direct_route(audio: &crate::settings::AudioSettings) -> bool {
    audio.backend_type == Some(crate::backend::AudioBackendType::Alsa)
        && audio.alsa_plugin.unwrap_or(crate::backend::AlsaPlugin::Hw)
            != crate::backend::AlsaPlugin::Pcm
        && audio
            .output_device
            .as_deref()
            .is_some_and(AlsaDirectStream::is_hw_device)
}

#[cfg(not(target_os = "linux"))]
impl AlsaDirectStream {
    pub fn new(_device_id: &str, _sample_rate: u32, _channels: u16) -> Result<Self, String> {
        Err("ALSA Direct is only available on Linux".to_string())
    }

    pub fn write(&self, _samples: &[i16]) -> Result<(), String> {
        Err("ALSA Direct is only available on Linux".to_string())
    }

    pub fn write_f32(&self, _samples: &[f32]) -> Result<(), String> {
        Err("ALSA Direct is only available on Linux".to_string())
    }

    pub fn drain(&self) -> Result<(), String> {
        Ok(())
    }

    pub fn stop(&self) -> Result<(), String> {
        Ok(())
    }

    pub fn sample_rate(&self) -> u32 {
        44100
    }

    pub fn channels(&self) -> u16 {
        2
    }

    pub fn playback_delay_frames(&self) -> Result<u64, String> {
        Ok(0)
    }

    /// Check if device is a bit-perfect hardware device (always false on non-Linux)
    pub fn is_hw_device(_device_id: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::uses_alsa_direct_route;
    use crate::alsa_hardware_volume::mixer_ctl_name;
    use crate::backend::{AlsaPlugin, AudioBackendType};
    use crate::settings::AudioSettings;

    #[test]
    fn mixer_ctl_name_maps_card_forms() {
        // HiFiBerry Digi S/PDIF (#331/#659) and other plugin aliases.
        assert_eq!(
            mixer_ctl_name("iec958:CARD=sndrpihifiberry,DEV=0"),
            "hw:CARD=sndrpihifiberry"
        );
        assert_eq!(mixer_ctl_name("hw:CARD=C20,DEV=0"), "hw:CARD=C20");
        assert_eq!(mixer_ctl_name("front:CARD=PCH,DEV=0"), "hw:CARD=PCH");
        assert_eq!(mixer_ctl_name("hdmi:CARD=NVidia,DEV=1"), "hw:CARD=NVidia");
        assert_eq!(
            mixer_ctl_name("sysdefault:CARD=DacMagic,DEV=0"),
            "hw:CARD=DacMagic"
        );
        // CARD= in a non-first position.
        assert_eq!(mixer_ctl_name("hw:DEV=0,CARD=DacMagic"), "hw:CARD=DacMagic");
    }

    #[test]
    fn mixer_ctl_name_numeric_and_fallback() {
        assert_eq!(mixer_ctl_name("hw:1,0"), "hw:1");
        assert_eq!(mixer_ctl_name("plughw:2"), "hw:2");
        // No CARD= and no numeric arg → unchanged.
        assert_eq!(mixer_ctl_name("default"), "default");
        assert_eq!(mixer_ctl_name("pulse"), "pulse");
    }

    #[test]
    fn direct_route_requires_alsa_direct_device_and_non_pcm_plugin() {
        let mut audio = AudioSettings::default();
        assert!(!uses_alsa_direct_route(&audio));

        audio.backend_type = Some(AudioBackendType::Alsa);
        audio.output_device = Some("front:CARD=USB,DEV=0".to_string());
        audio.alsa_plugin = Some(AlsaPlugin::Hw);
        assert_eq!(
            uses_alsa_direct_route(&audio),
            cfg!(target_os = "linux")
        );

        audio.alsa_plugin = Some(AlsaPlugin::PlugHw);
        assert_eq!(
            uses_alsa_direct_route(&audio),
            cfg!(target_os = "linux")
        );

        audio.alsa_plugin = Some(AlsaPlugin::Pcm);
        assert!(!uses_alsa_direct_route(&audio));

        audio.alsa_plugin = Some(AlsaPlugin::Hw);
        audio.output_device = Some("sysdefault:CARD=USB".to_string());
        assert!(!uses_alsa_direct_route(&audio));

        audio.output_device = None;
        assert!(!uses_alsa_direct_route(&audio));
    }
}

/// Forwards to the inherent methods, which is all this is: no behaviour is
/// added, moved or wrapped. See `backend::DirectSink`.
impl crate::backend::DirectSink for AlsaDirectStream {
    fn write_f32(&self, samples: &[f32]) -> Result<(), String> {
        AlsaDirectStream::write_f32(self, samples)
    }
    #[cfg(target_os = "linux")]
    fn write_f32_interruptible(&self, samples: &[f32], cancel: &AtomicBool)
        -> Result<usize, crate::backend::DirectWriteError> {
        self.write_f32_with_cancel(samples, cancel)
    }
    #[cfg(target_os = "linux")]
    fn drain_interruptible(&self, cancel: &AtomicBool) -> Result<(), String> {
        self.drain_with_cancel(cancel)
    }
    fn drain(&self) -> Result<(), String> {
        AlsaDirectStream::drain(self)
    }
    fn stop(&self) -> Result<(), String> {
        AlsaDirectStream::stop(self)
    }
    fn sample_rate(&self) -> u32 {
        AlsaDirectStream::sample_rate(self)
    }
    fn channels(&self) -> u16 {
        AlsaDirectStream::channels(self)
    }
    fn playback_delay_frames(&self) -> Result<u64, String> {
        AlsaDirectStream::playback_delay_frames(self)
    }
    fn log_label(&self) -> &'static str {
        "ALSA Direct Engine"
    }
    #[cfg(target_os = "linux")]
    fn alsa_device_id(&self) -> Option<&str> {
        Some(self.device_id())
    }
    /// Linux only, because the inherent method is: the non-Linux stub of this
    /// type has no mixer at all, and there the trait default refusal is the
    /// honest answer.
    #[cfg(target_os = "linux")]
    fn set_hardware_volume(
        &self,
        control: &crate::alsa_hardware_volume::AlsaMixerControlId,
        volume: f32,
    ) -> Result<(), String> {
        AlsaDirectStream::set_hardware_volume(self, control, volume)
    }
}
