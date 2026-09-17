//! Start-gain resolution for volume normalization — runs on the audio thread
//! when a source is about to be appended, so the FIRST sample already carries
//! the right level. Order: a stored row (any source) > ReplayGain tags in the
//! bytes (stored for next time) > unity while the live analyser measures.

use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};

use qbz_audio::loudness_cache::{gain_for, lufs_from_replaygain, LoudnessCache, LoudnessSource};
use qbz_audio::{extract_replaygain, AudioSettings, ReplayGainData};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StartGain {
    pub gain: f32,
    /// `true` = a real figure; `false` = unity placeholder, the analyser will set it.
    pub known: bool,
    pub source: Option<LoudnessSource>,
}

/// Everything a seeding site needs for one track: the initial gain for
/// `DynamicAmplify` (also the published normalization gain), the shared
/// atomic the analyser writes, and the analyser's own inputs. The `None`s
/// mean normalization is off — the bit-perfect path, untouched.
#[derive(Debug, Clone)]
pub(crate) struct StartGainPlan {
    pub normalization: Option<f32>,
    pub gain_atomic: Option<Arc<AtomicU32>>,
    pub target_lufs: Option<f32>,
    pub known_gain: bool,
    pub prevent_clipping: bool,
}

pub(crate) fn plan_start_gain(
    settings: &Mutex<AudioSettings>,
    cache: &LoudnessCache,
    track_id: u64,
    tagged_bytes: Option<&[u8]>,
) -> StartGainPlan {
    let norm = settings
        .lock()
        .ok()
        .filter(|s| s.normalization_enabled)
        .map(|s| (s.normalization_target_lufs, s.normalization_prevent_clipping));
    let Some((target_lufs, prevent_clipping)) = norm else {
        return StartGainPlan {
            normalization: None,
            gain_atomic: None,
            target_lufs: None,
            known_gain: false,
            prevent_clipping: true,
        };
    };
    let start = resolve_start_gain(cache, track_id, tagged_bytes, target_lufs, prevent_clipping);
    log::info!(
        "Normalization: track {} starts at gain {:.4} ({})",
        track_id,
        start.gain,
        start.source.map(|s| s.as_str()).unwrap_or("unknown, measuring")
    );
    StartGainPlan {
        normalization: Some(start.gain),
        gain_atomic: Some(Arc::new(AtomicU32::new(start.gain.to_bits()))),
        target_lufs: Some(target_lufs),
        known_gain: start.known,
        prevent_clipping,
    }
}

pub(crate) fn resolve_start_gain(
    cache: &LoudnessCache,
    track_id: u64,
    tagged_bytes: Option<&[u8]>,
    target_lufs: f32,
    prevent_clipping: bool,
) -> StartGain {
    if let Some((gain, source)) = cache.resolve_gain(track_id, target_lufs, prevent_clipping) {
        return StartGain {
            gain,
            known: true,
            source: Some(source),
        };
    }
    if let Some(rg) = tagged_bytes.and_then(extract_replaygain) {
        return from_replaygain(cache, track_id, &rg, target_lufs, prevent_clipping);
    }
    StartGain {
        gain: 1.0,
        known: false,
        source: None,
    }
}

fn from_replaygain(
    cache: &LoudnessCache,
    track_id: u64,
    rg: &ReplayGainData,
    target_lufs: f32,
    prevent_clipping: bool,
) -> StartGain {
    let lufs = lufs_from_replaygain(rg.gain_db);
    let peak = rg.peak.filter(|p| *p > 0.0);
    // A tag is a full-track figure: keep it so the next play needs no probe.
    cache.store(track_id, lufs, peak, LoudnessSource::ReplayGain);
    StartGain {
        gain: gain_for(lufs, peak, target_lufs, prevent_clipping),
        known: true,
        source: Some(LoudnessSource::ReplayGain),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn a_stored_row_wins_and_an_unknown_track_starts_at_unity() {
        let cache = LoudnessCache::in_memory().unwrap();
        assert_eq!(
            resolve_start_gain(&cache, 7, None, -14.0, true),
            StartGain {
                gain: 1.0,
                known: false,
                source: None
            }
        );
        cache.store(7, -12.0, Some(0.9), LoudnessSource::Ebur128Full);
        let s = resolve_start_gain(&cache, 7, None, -14.0, true);
        assert!(s.known && s.source == Some(LoudnessSource::Ebur128Full));
        assert!((s.gain - qbz_audio::db_to_linear(-2.0)).abs() < 1e-4);
    }

    #[test]
    fn replaygain_is_converted_and_persisted() {
        let cache = LoudnessCache::in_memory().unwrap();
        let rg = ReplayGainData {
            gain_db: -4.0,
            peak: Some(0.95),
        };
        let s = from_replaygain(&cache, 9, &rg, -14.0, true);
        assert!(s.known && s.source == Some(LoudnessSource::ReplayGain));
        // -18 - (-4) = -14 LUFS: already at target, gain 1.0.
        assert!((s.gain - 1.0).abs() < 1e-4);
        assert_eq!(cache.lookup(9).unwrap().lufs, Some(-14.0));
    }

    #[test]
    fn the_plan_is_off_without_normalization_and_seeds_the_atomic_with_it() {
        let cache = LoudnessCache::in_memory().unwrap();
        let off = Mutex::new(AudioSettings::default());
        let plan = plan_start_gain(&off, &cache, 7, None);
        assert!(plan.normalization.is_none() && plan.gain_atomic.is_none() && plan.target_lufs.is_none());

        let mut on = AudioSettings::default();
        on.normalization_enabled = true;
        on.normalization_target_lufs = -14.0;
        let on = Mutex::new(on);
        cache.store(7, -12.0, Some(0.9), LoudnessSource::Ebur128Full);
        let plan = plan_start_gain(&on, &cache, 7, None);
        assert!(plan.known_gain && plan.prevent_clipping && plan.target_lufs == Some(-14.0));
        let g = plan.normalization.unwrap();
        assert!((g - qbz_audio::db_to_linear(-2.0)).abs() < 1e-4);
        assert_eq!(f32::from_bits(plan.gain_atomic.unwrap().load(Ordering::Relaxed)), g);
    }
}
