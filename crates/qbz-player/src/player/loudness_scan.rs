//! Full-track pre-analysis over cached bytes: a second, silent decode (the
//! cast shadow decoder's `InMemorySource::from_shared`) fed into
//! `qbz_audio::measure_loudness`. Blocking; call from `spawn_blocking`.

use std::sync::Arc;

use super::streaming_source::InMemorySource;

pub(crate) fn measure_bytes(bytes: &Arc<Vec<u8>>) -> Result<qbz_audio::TrackLoudness, String> {
    let mut source = InMemorySource::from_shared(Arc::clone(bytes))?;
    let (rate, channels) = (source.sample_rate(), source.channels());
    qbz_audio::measure_loudness(&mut source, rate, channels)
}
