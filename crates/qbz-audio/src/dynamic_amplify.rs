//! Dynamic gain wrapper for real-time volume normalization.
//!
//! Reads gain from a shared `Arc<AtomicU32>` (f32 stored as bits) and applies
//! it to each sample. When the gain value changes, a [`RAMP_MS`] linear ramp
//! smooths the transition (perceptual: a late correction glides instead of
//! stepping).
//!
//! When the atomic holds 0.0 (gain not yet computed), the wrapper stays at
//! the `initial_gain` provided at construction.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rodio::Source;

/// Ramp for a gain CHANGE mid-track. 50 ms de-clicked but read as a step;
/// 400 ms is a glide.
pub const RAMP_MS: u32 = 400;
/// Poll the shared atomic every N samples while not ramping (one relaxed
/// load per sample was 384 k loads/s at 192 kHz stereo).
const POLL_EVERY: u32 = 1024;

pub struct DynamicAmplify<S>
where
    S: Source<Item = f32>,
{
    inner: S,
    gain_atomic: Arc<AtomicU32>,
    /// Current applied gain (smoothly ramped)
    current_gain: f32,
    /// Target gain we're ramping toward
    target_gain: f32,
    /// Gain increment per sample during ramp
    ramp_step: f32,
    /// Samples remaining in the current ramp
    ramp_remaining: u32,
    /// Number of samples in a [`RAMP_MS`] ramp at the current sample rate
    ramp_samples: u32,
    /// Samples left before the next atomic poll (while not ramping)
    poll_countdown: u32,
}

impl<S> DynamicAmplify<S>
where
    S: Source<Item = f32>,
{
    pub fn new(source: S, gain_atomic: Arc<AtomicU32>, initial_gain: f32) -> Self {
        let sample_rate = source.sample_rate().get();
        let channels = source.channels().get() as u32;
        // Ramp in total samples (all channels)
        let ramp_samples = (sample_rate * channels * RAMP_MS) / 1000;

        Self {
            inner: source,
            gain_atomic,
            current_gain: initial_gain,
            target_gain: initial_gain,
            ramp_step: 0.0,
            ramp_remaining: 0,
            ramp_samples,
            poll_countdown: 0,
        }
    }

    /// Check for a new gain value and start a ramp if it changed.
    #[inline]
    fn poll_gain(&mut self) {
        let bits = self.gain_atomic.load(Ordering::Relaxed);
        let new_gain = f32::from_bits(bits);

        // 0.0 means "not yet computed" — stay at current gain
        if new_gain == 0.0 {
            return;
        }

        // Only start a ramp if the target actually changed
        if (new_gain - self.target_gain).abs() > f32::EPSILON {
            self.target_gain = new_gain;
            if self.ramp_samples > 0 {
                self.ramp_step = (self.target_gain - self.current_gain) / self.ramp_samples as f32;
                self.ramp_remaining = self.ramp_samples;
            } else {
                self.current_gain = self.target_gain;
                self.ramp_remaining = 0;
            }
        }
    }
}

impl<S> Iterator for DynamicAmplify<S>
where
    S: Source<Item = f32>,
{
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        // Poll for a new gain every POLL_EVERY samples while not ramping
        // (the ramp_remaining check is essentially free).
        if self.ramp_remaining == 0 {
            if self.poll_countdown == 0 {
                self.poll_gain();
                self.poll_countdown = POLL_EVERY;
            } else {
                self.poll_countdown -= 1;
            }
        }

        let sample = self.inner.next()?;

        if self.ramp_remaining > 0 {
            self.current_gain += self.ramp_step;
            self.ramp_remaining -= 1;
            if self.ramp_remaining == 0 {
                // Snap to target at end of ramp to avoid float drift
                self.current_gain = self.target_gain;
            }
        }

        Some(sample * self.current_gain)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S> Source for DynamicAmplify<S>
where
    S: Source<Item = f32>,
{
    #[inline]
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    #[inline]
    fn channels(&self) -> std::num::NonZero<u16> {
        self.inner.channels()
    }

    #[inline]
    fn sample_rate(&self) -> std::num::NonZero<u32> {
        self.inner.sample_rate()
    }

    #[inline]
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZero;

    struct Ones {
        rate: u32,
        ch: u16,
    }
    impl Iterator for Ones {
        type Item = f32;
        fn next(&mut self) -> Option<f32> {
            Some(1.0)
        }
    }
    impl Source for Ones {
        fn current_span_len(&self) -> Option<usize> {
            None
        }
        fn channels(&self) -> NonZero<u16> {
            NonZero::new(self.ch).unwrap()
        }
        fn sample_rate(&self) -> NonZero<u32> {
            NonZero::new(self.rate).unwrap()
        }
        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }

    #[test]
    fn the_initial_gain_applies_from_the_first_sample_without_a_ramp() {
        let atomic = Arc::new(AtomicU32::new(0.5f32.to_bits()));
        let mut amp = DynamicAmplify::new(Ones { rate: 1000, ch: 1 }, atomic, 0.5);
        assert_eq!(amp.next(), Some(0.5));
        assert_eq!(amp.next(), Some(0.5));
    }

    #[test]
    fn a_gain_change_glides_over_the_ramp_and_lands_exactly() {
        let atomic = Arc::new(AtomicU32::new(0.5f32.to_bits()));
        let mut amp = DynamicAmplify::new(Ones { rate: 1000, ch: 1 }, atomic.clone(), 0.5);
        atomic.store(0.25f32.to_bits(), Ordering::Relaxed);
        let ramp = (1000 * RAMP_MS) / 1000; // 400 samples at 1 kHz mono
        let out: Vec<f32> = (0..ramp as usize).map(|_| amp.next().unwrap()).collect();
        assert!(out[0] < 0.5 && out[0] > 0.25, "first sample already moving: {}", out[0]);
        assert!((out[199] - 0.375).abs() < 0.01, "halfway through the ramp: {}", out[199]);
        assert_eq!(out[ramp as usize - 1], 0.25, "snaps to the target at the end");
        assert_eq!(amp.next(), Some(0.25));
    }
}
