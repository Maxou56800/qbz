//! Analyzer tap — captures audio samples for loudness analysis.
//!
//! Sits in the audio pipeline as a transparent `Source<Item = f32>` wrapper.
//! Batches samples and sends them to the loudness analyzer thread via a bounded
//! channel. Uses `try_send` so it never blocks the audio thread — if the channel
//! is full, the batch is silently dropped (graceful degradation).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Duration;

use rodio::Source;

/// Messages sent from the audio pipeline to the loudness analyzer thread.
pub enum AnalyzerMessage {
    /// A batch of interleaved f32 samples with its absolute track frame.
    Samples { start_frame: u64, samples: Vec<f32> },
    /// The source reached its first audible frame. Emitting this from the tap,
    /// rather than when a gapless source is queued, preserves the true track
    /// boundary inside one output stream for both analyzers.
    NewTrack(AnalyzerWaveformTrack),
    /// Seek occurred — reset accumulated samples but keep current gain.
    Reset,
    /// Shut down the analyzer thread.
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct AnalyzerWaveformTrack {
    pub track_id: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_secs: u64,
    pub start_frame: u64,
    pub target_lufs: Option<f32>,
    /// Shared gain atomic — loudness analyzer writes, DynamicAmplify reads.
    pub gain_atomic: Option<Arc<AtomicU32>>,
    /// A start gain was resolved before the first sample (cache / ReplayGain):
    /// the live analyser must not overwrite it.
    pub known_gain: bool,
    pub prevent_clipping: bool,
}

const BATCH_SIZE: usize = 4096;

pub struct AnalyzerTap<S>
where
    S: Source<Item = f32>,
{
    inner: S,
    sender: SyncSender<AnalyzerMessage>,
    enabled: Arc<AtomicBool>,
    buffer: Vec<f32>,
    channels: u16,
    sample_cursor: u64,
    batch_start_frame: u64,
    waveform_track: Option<AnalyzerWaveformTrack>,
    waveform_boundary_sent: bool,
}

impl<S> AnalyzerTap<S>
where
    S: Source<Item = f32>,
{
    pub fn new(source: S, sender: SyncSender<AnalyzerMessage>, enabled: Arc<AtomicBool>) -> Self {
        let channels = source.channels().get();
        Self {
            inner: source,
            sender,
            enabled,
            buffer: Vec::with_capacity(BATCH_SIZE),
            channels,
            sample_cursor: 0,
            batch_start_frame: 0,
            waveform_track: None,
            waveform_boundary_sent: true,
        }
    }

    pub fn new_with_waveform(
        source: S,
        sender: SyncSender<AnalyzerMessage>,
        enabled: Arc<AtomicBool>,
        waveform_track: Option<AnalyzerWaveformTrack>,
        start_frame: u64,
    ) -> Self {
        let channels = source.channels().get();
        Self {
            inner: source,
            sender,
            enabled,
            buffer: Vec::with_capacity(BATCH_SIZE),
            channels,
            sample_cursor: start_frame.saturating_mul(channels as u64),
            batch_start_frame: start_frame,
            waveform_boundary_sent: waveform_track.is_none(),
            waveform_track,
        }
    }

    #[inline]
    fn announce_waveform_track(&mut self) {
        if self.waveform_boundary_sent
            || (!self.enabled.load(Ordering::Relaxed)
                && !crate::seek_waveform::seek_waveform_enabled())
        {
            return;
        }
        let Some(track) = self.waveform_track.clone() else {
            self.waveform_boundary_sent = true;
            return;
        };
        if self
            .sender
            .try_send(AnalyzerMessage::NewTrack(track))
            .is_ok()
        {
            self.waveform_boundary_sent = true;
        }
    }

    #[inline]
    fn flush_if_full(&mut self) {
        // Keep every message frame-aligned. 4096 is not divisible by every
        // legal channel count (notably 3), and splitting an interleaved frame
        // would shift both RMS analyzers at the next batch boundary.
        if self.buffer.len() >= BATCH_SIZE && self.buffer.len() % self.channels.max(1) as usize == 0
        {
            self.flush_buffer();
        }
    }

    #[inline]
    fn flush_buffer(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        let batch = std::mem::replace(&mut self.buffer, Vec::with_capacity(BATCH_SIZE));
        // Non-blocking send — drop batch if channel is full. The absolute
        // frame on the next successful batch preserves the timeline gap.
        let _ = self.sender.try_send(AnalyzerMessage::Samples {
            start_frame: self.batch_start_frame,
            samples: batch,
        });
        self.batch_start_frame = self.sample_cursor / self.channels.max(1) as u64;
    }
}

impl<S> Iterator for AnalyzerTap<S>
where
    S: Source<Item = f32>,
{
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let Some(sample) = self.inner.next() else {
            self.flush_buffer();
            return None;
        };

        self.announce_waveform_track();
        let capture =
            self.enabled.load(Ordering::Relaxed) || crate::seek_waveform::seek_waveform_enabled();
        if capture {
            if self.buffer.is_empty() {
                self.batch_start_frame = self.sample_cursor / self.channels.max(1) as u64;
            }
            self.buffer.push(sample);
            self.sample_cursor = self.sample_cursor.saturating_add(1);
            self.flush_if_full();
        } else {
            self.sample_cursor = self.sample_cursor.saturating_add(1);
        }

        Some(sample)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S> Source for AnalyzerTap<S>
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
    use rodio::buffer::SamplesBuffer;
    use std::num::NonZero;
    use std::sync::mpsc;

    fn track(track_id: u64) -> AnalyzerWaveformTrack {
        AnalyzerWaveformTrack {
            track_id,
            sample_rate: 48_000,
            channels: 2,
            duration_secs: 1,
            start_frame: 0,
            target_lufs: None,
            gain_atomic: None,
            known_gain: false,
            prevent_clipping: true,
        }
    }

    fn source(samples: Vec<f32>) -> SamplesBuffer {
        SamplesBuffer::new(
            NonZero::new(2u16).unwrap(),
            NonZero::new(48_000u32).unwrap(),
            samples,
        )
    }

    #[test]
    fn boundary_is_emitted_only_when_gapless_source_becomes_audible() {
        let (tx, rx) = mpsc::sync_channel(8);
        let enabled = Arc::new(AtomicBool::new(true));
        let mut first = AnalyzerTap::new_with_waveform(
            source(vec![0.1, 0.1]),
            tx.clone(),
            enabled.clone(),
            Some(track(41)),
            0,
        );
        assert!(matches!(first.next(), Some(_)));
        assert!(
            matches!(rx.recv().unwrap(), AnalyzerMessage::NewTrack(info) if info.track_id == 41)
        );
        let _: Vec<_> = first.collect();
        assert!(matches!(
            rx.recv().unwrap(),
            AnalyzerMessage::Samples { start_frame: 0, .. }
        ));

        let mut queued =
            AnalyzerTap::new_with_waveform(source(vec![0.2, 0.2]), tx, enabled, Some(track(42)), 0);
        assert!(rx.try_recv().is_err());
        assert!(matches!(queued.next(), Some(_)));
        assert!(
            matches!(rx.recv().unwrap(), AnalyzerMessage::NewTrack(info) if info.track_id == 42)
        );
    }

    #[test]
    fn eof_flushes_partial_batch_with_absolute_start_frame() {
        let (tx, rx) = mpsc::sync_channel(8);
        let enabled = Arc::new(AtomicBool::new(true));
        let tap = AnalyzerTap::new_with_waveform(
            source(vec![0.3, 0.3, 0.4, 0.4]),
            tx,
            enabled,
            Some(track(51)),
            125,
        );
        let _: Vec<_> = tap.collect();
        assert!(
            matches!(rx.recv().unwrap(), AnalyzerMessage::NewTrack(info) if info.track_id == 51)
        );
        match rx.recv().unwrap() {
            AnalyzerMessage::Samples {
                start_frame,
                samples,
            } => {
                assert_eq!(start_frame, 125);
                assert_eq!(samples, vec![0.3, 0.3, 0.4, 0.4]);
            }
            _ => panic!("expected samples"),
        }
    }
}
