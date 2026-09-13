//! Full-track loudness measurement over an already-decoded sample stream:
//! EBU R128 integrated loudness + true peak. Pure; the caller owns the
//! decoder (qbz-player's prefetch pre-analysis) and the thread.

use ebur128::{EbuR128, Mode};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackLoudness {
    pub lufs: f32,
    /// Linear true peak, `None` when not measurable.
    pub true_peak: Option<f32>,
    pub frames: u64,
}

/// Feed interleaved f32 samples until the iterator ends. Errors on a silent
/// track (no integrated loudness) so the caller stores nothing.
pub fn measure(
    samples: &mut dyn Iterator<Item = f32>,
    sample_rate: u32,
    channels: u16,
) -> Result<TrackLoudness, String> {
    let ch = channels.max(1) as usize;
    let mut meter = EbuR128::new(ch as u32, sample_rate, Mode::I | Mode::TRUE_PEAK)
        .map_err(|e| format!("ebur128 init: {e}"))?;
    let mut buf: Vec<f32> = Vec::with_capacity(4096 * ch);
    let mut frames = 0u64;
    loop {
        buf.clear();
        for _ in 0..4096 * ch {
            match samples.next() {
                Some(s) => buf.push(s),
                None => break,
            }
        }
        let whole = buf.len() - buf.len() % ch;
        if whole == 0 {
            break;
        }
        meter
            .add_frames_f32(&buf[..whole])
            .map_err(|e| format!("ebur128 feed: {e}"))?;
        frames += (whole / ch) as u64;
    }
    let lufs = meter
        .loudness_global()
        .map_err(|e| format!("ebur128 loudness: {e}"))?;
    if !lufs.is_finite() {
        return Err("silent track (no integrated loudness)".into());
    }
    let true_peak = (0..ch as u32)
        .filter_map(|c| meter.true_peak(c).ok())
        .fold(0.0f64, f64::max);
    Ok(TrackLoudness {
        lufs: lufs as f32,
        true_peak: (true_peak > 0.0).then_some(true_peak as f32),
        frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 3 s of a 1 kHz stereo sine at amplitude 0.5: about -0.7 + 10·log10(2 × 0.125)
    /// = -6.7 LUFS (K-weighting is ~flat at 1 kHz), true peak 0.5.
    #[test]
    fn a_known_sine_measures_where_r128_says_it_should() {
        let (rate, secs) = (48_000u32, 3u32);
        let mut t = 0u64;
        let mut samples = std::iter::from_fn(move || {
            if t >= (rate * secs) as u64 * 2 {
                return None;
            }
            let frame = (t / 2) as f32 / rate as f32;
            t += 1;
            Some(0.5 * (2.0 * std::f32::consts::PI * 1000.0 * frame).sin())
        });
        let m = measure(&mut samples, rate, 2).unwrap();
        assert_eq!(m.frames, (rate * secs) as u64);
        assert!((-8.5..=-5.0).contains(&m.lufs), "lufs {}", m.lufs);
        let tp = m.true_peak.unwrap();
        assert!((tp - 0.5).abs() < 0.03, "true peak {tp}");
    }
}
