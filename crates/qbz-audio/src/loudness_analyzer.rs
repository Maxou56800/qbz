//! Background loudness analyzer thread.
//!
//! Long-lived thread that receives decoded audio samples from `AnalyzerTap`,
//! computes EBU R128 integrated LUFS + true peak, and updates a shared
//! `Arc<AtomicU32>` gain value that `DynamicAmplify` reads.
//!
//! - Start gain comes from the player (cache / ReplayGain) when known; else
//!   the first measurement after ~10 s of audio sets it ONCE per track
//! - Refinement every ~5 s feeds the cache only (≥ 30 s partial; the last
//!   window of the track is stored as the full-track figure)

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread;

use ebur128::{EbuR128, Mode};

use super::analyzer_tap::AnalyzerMessage;
use super::loudness_cache::{gain_for, LoudnessCache, LoudnessSource};
use super::seek_waveform::{SeekWaveformAccumulator, SeekWaveformCache};

/// A live window shorter than this is not worth caching: a skip at 12 s must
/// not become "the loudness of the track".
const MIN_CACHE_SECS: u64 = 30;
/// The window that reaches this close to the end of the track is the
/// full-track measurement.
const END_WINDOW_SECS: u64 = 2;

pub struct LoudnessAnalyzer;

impl LoudnessAnalyzer {
    /// Spawn the analyzer thread. Returns the join handle.
    ///
    /// The thread blocks on `rx.recv()` when idle — zero CPU usage between tracks.
    pub fn spawn(
        rx: Receiver<AnalyzerMessage>,
        cache: Arc<LoudnessCache>,
    ) -> thread::JoinHandle<()> {
        thread::Builder::new()
            .name("loudness-analyzer".into())
            .spawn(move || {
                log::info!("[LoudnessAnalyzer] Thread started");
                Self::run(rx, cache);
                log::info!("[LoudnessAnalyzer] Thread exiting");
            })
            .expect("Failed to spawn loudness analyzer thread")
    }

    fn run(rx: Receiver<AnalyzerMessage>, cache: Arc<LoudnessCache>) {
        let mut state: Option<AnalyzerState> = None;
        // Default-off really is idle: do not touch the waveform database
        // until the preference is enabled for an audible track.
        let mut waveform_cache = None;
        let mut waveform: Option<SeekWaveformAccumulator> = None;
        let mut current_track = None;

        loop {
            let msg = match rx.recv() {
                Ok(msg) => msg,
                Err(_) => {
                    log::info!("[LoudnessAnalyzer] Channel closed, shutting down");
                    break;
                }
            };

            match msg {
                AnalyzerMessage::NewTrack(track) => {
                    state = match (track.target_lufs, track.gain_atomic.clone()) {
                        (Some(target_lufs), Some(gain_atomic)) => {
                            log::info!(
                                "[LoudnessAnalyzer] New track {} ({}Hz, {}ch, target {:.1} LUFS, start gain {})",
                                track.track_id,
                                track.sample_rate,
                                track.channels,
                                target_lufs,
                                if track.known_gain { "known" } else { "unknown, measuring" }
                            );
                            // The player already resolved the start gain
                            // (cache / ReplayGain); this thread only ever
                            // fills a gap and feeds the cache.
                            let mut analyzer = AnalyzerState::new(
                                track.track_id,
                                track.sample_rate,
                                track.channels,
                                target_lufs,
                                track.known_gain,
                                track.prevent_clipping,
                                track.duration_secs,
                            );
                            analyzer.gain_atomic = Some(gain_atomic);
                            Some(analyzer)
                        }
                        _ => None,
                    };
                    waveform = if super::seek_waveform::seek_waveform_enabled() {
                        ensure_waveform_cache(&mut waveform_cache);
                        Some(SeekWaveformAccumulator::begin(
                            track.track_id,
                            track.sample_rate,
                            track.channels,
                            track.duration_secs,
                            waveform_cache.as_ref(),
                        ))
                    } else {
                        None
                    };
                    current_track = Some(track);
                }
                AnalyzerMessage::Samples {
                    start_frame,
                    samples,
                } => {
                    if let Some(ref mut s) = state {
                        s.feed_samples(&samples, &cache);
                    }
                    if super::seek_waveform::seek_waveform_enabled() {
                        ensure_waveform_cache(&mut waveform_cache);
                        if waveform.is_none() {
                            waveform = current_track.as_ref().map(|track| {
                                SeekWaveformAccumulator::begin(
                                    track.track_id,
                                    track.sample_rate,
                                    track.channels,
                                    track.duration_secs,
                                    waveform_cache.as_ref(),
                                )
                            });
                        }
                        if let Some(ref mut accumulator) = waveform {
                            accumulator.feed(start_frame, &samples, waveform_cache.as_ref());
                        }
                    } else {
                        waveform = None;
                    }
                }
                AnalyzerMessage::Reset => {
                    if let Some(ref mut s) = state {
                        log::info!("[LoudnessAnalyzer] Reset (seek) — keeping current gain");
                        s.reset_analyzer();
                    }
                }
                AnalyzerMessage::Shutdown => {
                    log::info!("[LoudnessAnalyzer] Shutdown requested");
                    break;
                }
            }
        }
    }
}

fn ensure_waveform_cache(cache: &mut Option<SeekWaveformCache>) {
    if cache.is_none() {
        *cache = SeekWaveformCache::open()
            .map_err(|error| log::warn!("[SeekWaveform] {error}"))
            .ok();
    }
}

struct AnalyzerState {
    track_id: u64,
    target_lufs: f32,
    ebur128: EbuR128,
    channels: u16,
    sample_rate: u32,
    /// Shared gain atomic — written by us, read by DynamicAmplify
    gain_atomic: Option<Arc<AtomicU32>>,
    /// Total samples fed since last reset
    samples_fed: u64,
    /// Total samples fed at last measurement
    samples_at_last_measure: u64,
    /// The first measurement of the current window has been taken.
    first_measure_done: bool,
    /// A start gain was resolved before the first sample (cache /
    /// ReplayGain): the live gain is never touched here.
    known_gain: bool,
    /// The live gain has been written once for this track. A seek does not
    /// reset it: the level after a seek is the level before it.
    live_gain_applied: bool,
    prevent_clipping: bool,
    /// Track length for the end-of-track trigger; 0 = unknown.
    duration_secs: u64,
    /// The end-of-track measurement of the current window has been taken.
    end_measured: bool,
    /// Dynamic thresholds based on actual sample rate and channels
    initial_threshold: u64,
    refinement_interval: u64,
}

impl AnalyzerState {
    fn new(
        track_id: u64,
        sample_rate: u32,
        channels: u16,
        target_lufs: f32,
        known_gain: bool,
        prevent_clipping: bool,
        duration_secs: u64,
    ) -> Self {
        let ebur128 = EbuR128::new(channels as u32, sample_rate, Mode::I | Mode::TRUE_PEAK)
            .expect("Failed to create EbuR128 instance");

        // Scale thresholds to actual sample rate and channel count
        let samples_per_second = sample_rate as u64 * channels as u64;
        let initial_threshold = samples_per_second * 10; // 10 seconds
        let refinement_interval = samples_per_second * 5; // 5 seconds

        Self {
            track_id,
            target_lufs,
            ebur128,
            channels,
            sample_rate,
            gain_atomic: None,
            samples_fed: 0,
            samples_at_last_measure: 0,
            first_measure_done: false,
            known_gain,
            live_gain_applied: false,
            prevent_clipping,
            duration_secs,
            end_measured: false,
            initial_threshold,
            refinement_interval,
        }
    }

    /// Reset the EBU R128 window (e.g. after a seek). A seek restarts the
    /// window, never the gain: `known_gain` and `live_gain_applied` stay.
    fn reset_analyzer(&mut self) {
        self.ebur128 = EbuR128::new(self.channels as u32, self.sample_rate, Mode::I | Mode::TRUE_PEAK)
            .expect("Failed to create EbuR128 instance");
        self.samples_fed = 0;
        self.samples_at_last_measure = 0;
        self.first_measure_done = false;
        self.end_measured = false;
    }

    fn seconds_fed(&self) -> u64 {
        self.samples_fed / (self.sample_rate as u64 * self.channels as u64).max(1)
    }

    /// Feed samples to the EBU R128 analyzer and possibly update gain.
    fn feed_samples(&mut self, samples: &[f32], cache: &LoudnessCache) {
        // Feed interleaved samples as frames
        let frame_count = samples.len() / self.channels as usize;
        if frame_count == 0 {
            return;
        }

        if let Err(e) = self.ebur128.add_frames_f32(samples) {
            log::warn!("[LoudnessAnalyzer] Error feeding samples: {}", e);
            return;
        }

        self.samples_fed += samples.len() as u64;

        // Check if it's time to measure: the 10 s / 5 s cadence, plus ONE
        // extra measurement when the window reaches the end of the track,
        // so the full-track figure is what the cache keeps.
        let near_end = self.duration_secs > 0
            && !self.end_measured
            && self.seconds_fed() + END_WINDOW_SECS >= self.duration_secs;
        let should_measure = near_end
            || if !self.first_measure_done {
                self.samples_fed >= self.initial_threshold
            } else {
                self.samples_fed - self.samples_at_last_measure >= self.refinement_interval
            };

        if should_measure {
            if near_end {
                self.end_measured = true;
            }
            self.measure_and_update(cache);
        }
    }

    fn measure_and_update(&mut self, cache: &LoudnessCache) {
        let loudness = match self.ebur128.loudness_global() {
            Ok(l) => l,
            Err(e) => {
                log::warn!("[LoudnessAnalyzer] Failed to get loudness: {}", e);
                return;
            }
        };

        // -inf means silence — don't adjust
        if loudness.is_infinite() || loudness.is_nan() {
            log::debug!(
                "[LoudnessAnalyzer] Track {}: loudness is {:?}, skipping",
                self.track_id,
                loudness
            );
            return;
        }

        let measured_lufs = loudness as f32;
        let true_peak = (0..self.channels as u32)
            .filter_map(|c| self.ebur128.true_peak(c).ok())
            .fold(0.0f64, f64::max) as f32;
        let peak = (true_peak > 0.0).then_some(true_peak);
        let gain = gain_for(measured_lufs, peak, self.target_lufs, self.prevent_clipping);
        let secs = self.seconds_fed();

        log::info!(
            "[LoudnessAnalyzer] Track {} ({}): {:.1} LUFS, true peak {:.3}, target {:.1}, gain {:.4} after {}s",
            self.track_id,
            if self.first_measure_done { "refine" } else { "initial" },
            measured_lufs,
            true_peak,
            self.target_lufs,
            gain,
            secs
        );

        // The live gain moves at most ONCE per track, and only when nothing
        // was known at start: a later window must not re-level the song.
        if !self.known_gain && !self.live_gain_applied {
            if let Some(ref atomic) = self.gain_atomic {
                atomic.store(gain.to_bits(), Ordering::Relaxed);
            }
            self.live_gain_applied = true;
        }
        self.samples_at_last_measure = self.samples_fed;
        self.first_measure_done = true;

        // Cache policy: the last window of the track is the full-track
        // figure; anything from 30 s on is worth keeping as partial; a short
        // window (a skip at 12 s) is not.
        if self.duration_secs > 0 && secs + END_WINDOW_SECS >= self.duration_secs {
            self.store(cache, measured_lufs, peak, LoudnessSource::Ebur128Full);
        } else if secs >= MIN_CACHE_SECS {
            self.store(cache, measured_lufs, peak, LoudnessSource::Ebur128Partial);
        }
    }

    fn store(&self, cache: &LoudnessCache, lufs: f32, peak: Option<f32>, source: LoudnessSource) {
        cache.store(self.track_id, lufs, peak, source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_batch(rate: u32, frames: usize, t0: &mut u64) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for _ in 0..frames {
            let s = 0.1 * (2.0 * std::f32::consts::PI * 1000.0 * (*t0 as f32 / rate as f32)).sin();
            v.push(s);
            v.push(s);
            *t0 += 1;
        }
        v
    }

    fn feed_seconds(state: &mut AnalyzerState, cache: &LoudnessCache, rate: u32, secs: u32, t0: &mut u64) {
        let mut left = (rate * secs) as usize;
        while left > 0 {
            let n = left.min(2048);
            state.feed_samples(&sine_batch(rate, n, t0), cache);
            left -= n;
        }
    }

    #[test]
    fn unknown_start_writes_the_live_gain_once_and_caches_from_30s_and_at_the_end() {
        let cache = LoudnessCache::in_memory().unwrap();
        let atomic = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let mut s = AnalyzerState::new(7, 48_000, 2, -14.0, false, true, 40);
        s.gain_atomic = Some(atomic.clone());
        let mut t = 0u64;
        feed_seconds(&mut s, &cache, 48_000, 9, &mut t);
        assert_eq!(f32::from_bits(atomic.load(Ordering::Relaxed)), 1.0, "nothing before 10 s");
        feed_seconds(&mut s, &cache, 48_000, 2, &mut t);
        let first = f32::from_bits(atomic.load(Ordering::Relaxed));
        assert!(first > 1.0 && first < 2.0, "a -20 LUFS sine at -14 target is boosted: {first}");
        assert!(cache.lookup(7).is_none(), "an 11 s window is not cached");
        feed_seconds(&mut s, &cache, 48_000, 20, &mut t);
        assert_eq!(
            f32::from_bits(atomic.load(Ordering::Relaxed)),
            first,
            "refinements never touch the live gain"
        );
        assert_eq!(cache.lookup(7).unwrap().source, LoudnessSource::Ebur128Partial);
        feed_seconds(&mut s, &cache, 48_000, 8, &mut t);
        assert_eq!(
            cache.lookup(7).unwrap().source,
            LoudnessSource::Ebur128Full,
            "39 s of a 40 s track is the full measurement"
        );
        // A seek resets the window but not the latch.
        s.reset_analyzer();
        feed_seconds(&mut s, &cache, 48_000, 12, &mut t);
        assert_eq!(f32::from_bits(atomic.load(Ordering::Relaxed)), first);
    }

    #[test]
    fn a_known_start_gain_is_never_overwritten_by_the_live_analyser() {
        let cache = LoudnessCache::in_memory().unwrap();
        let atomic = Arc::new(AtomicU32::new(0.7f32.to_bits()));
        let mut s = AnalyzerState::new(7, 48_000, 2, -14.0, true, true, 40);
        s.gain_atomic = Some(atomic.clone());
        let mut t = 0u64;
        feed_seconds(&mut s, &cache, 48_000, 35, &mut t);
        assert_eq!(f32::from_bits(atomic.load(Ordering::Relaxed)), 0.7);
        assert_eq!(
            cache.lookup(7).unwrap().source,
            LoudnessSource::Ebur128Partial,
            "the window still lands in the cache"
        );
    }
}
