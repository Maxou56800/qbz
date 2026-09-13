//! Disk-backed playback for tracks that do not fit the live RAM budget.
//! Audio bytes and format are unchanged; only their storage and reader change.
use super::*;

pub(super) struct PrefetchGuard {
    cache: Arc<qbz_cache::AudioCache>,
    track_id: u64,
}
impl PrefetchGuard {
    pub(super) fn new(cache: Arc<qbz_cache::AudioCache>, track_id: u64) -> Self {
        cache.mark_fetching(track_id);
        Self { cache, track_id }
    }
}
impl Drop for PrefetchGuard {
    fn drop(&mut self) {
        self.cache.unmark_fetching(self.track_id);
    }
}

impl Player {
    pub(super) fn streaming_buffer(
        &self,
        config: StreamingConfig,
        total_size: Option<u64>,
    ) -> Result<(BufferedMediaSource, BufferWriter), String> {
        let total_size = total_size.filter(|size| *size > 0);
        let streaming_only = self
            .audio_settings
            .lock()
            .map(|s| s.streaming_only)
            .unwrap_or(false);
        let fits = total_size
            .and_then(|size| usize::try_from(size).ok())
            .is_some_and(|size| self.audio_cache.can_buffer_in_memory(size));
        if streaming_only || fits {
            return Ok(BufferedMediaSource::new(config, total_size));
        }
        let cache = self
            .audio_cache
            .get_playback_cache()
            .ok_or("Track exceeds the memory budget and playback disk cache is unavailable")?;
        let file = cache
            .create_spool()
            .map_err(|error| format!("Cannot create playback spool: {error}"))?;
        log::info!("[STREAM-STORAGE] disk-backed buffer, expected_bytes={total_size:?}");
        Ok(BufferedMediaSource::new_on_disk(config, total_size, file))
    }

    pub(super) async fn persist_stream(
        cache: Arc<qbz_cache::AudioCache>,
        writer: BufferWriter,
        track_id: u64,
    ) -> Result<(), String> {
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let size = writer.buffer_size();
            let admission = if let Some(data) = writer.complete_data_shared() {
                cache.insert_shared(track_id, data)
            } else {
                // The spool is copied in bounded chunks; never materialize an
                // oversized file just to pass it to the L2 writer.
                let saved = cache.get_playback_cache().is_some_and(|disk| {
                    disk.insert_from(track_id, size as u64, |file| writer.write_buffered_to(file))
                });
                if saved {
                    qbz_cache::CacheAdmission::Disk
                } else {
                    qbz_cache::CacheAdmission::Skipped
                }
            };
            log::info!("[PLAYBACK-CACHE] Track {track_id} admission: {admission:?} ({size} bytes)");
            Ok(())
        })
        .await
        .map_err(|error| format!("Playback cache task failed: {error}"))?
    }

    pub(super) fn cached_disk_source(
        &self,
        track_id: u64,
        quality: Quality,
    ) -> Result<Option<(Arc<BufferedMediaSource>, AudioMetadata, u64)>, String> {
        let Some(file) = self
            .audio_cache
            .get_playback_cache()
            .and_then(|cache| cache.open(track_id))
        else {
            return Ok(None);
        };
        let source = Arc::new(BufferedMediaSource::from_file(file).map_err(|e| e.to_string())?);
        let (meta, duration) = source_metadata(&source)?;
        let below = match quality {
            Quality::UltraHiRes => meta.bit_depth.unwrap_or(16) < 24 || meta.sample_rate <= 96_000,
            Quality::HiRes => meta.bit_depth.unwrap_or(16) < 24,
            _ => false,
        };
        if below {
            log::info!("[CACHE] Disk track {track_id} below requested {quality:?}; re-fetching");
            return Ok(None);
        }
        log::info!(
            "[CACHE HIT] Track {track_id} from DISK, {} bytes, {}Hz; no full-file RAM read",
            source.buffer_size(),
            meta.sample_rate
        );
        Ok(Some((source, meta, duration)))
    }

    pub(super) fn apply_completed_source(
        &self,
        source: Arc<BufferedMediaSource>,
        meta: AudioMetadata,
        duration_secs: u64,
        track_id: u64,
        start_position_secs: u64,
    ) -> Result<(), String> {
        let play_gen = self.state.current_play_generation();
        self.state
            .set_stream_quality(meta.sample_rate, meta.bit_depth.unwrap_or(16));
        self.state.begin_buffering(track_id, play_gen);
        self.tx
            .send(AudioCommand::PlayStreaming {
                content_length: source.buffer_size() as u64,
                source,
                track_id,
                sample_rate: meta.sample_rate,
                channels: meta.channels,
                duration_secs,
                start_position_secs,
                play_gen,
            })
            .map_err(|error| format!("Failed to send cached stream: {error}"))?;
        self.state.seal_stream_feeder();
        Ok(())
    }

    /// Legacy CDN fallback, consuming each HTTP chunk directly into its chosen
    /// storage. Kept separate from the Vec API used by external renderers.
    pub(super) async fn download_buffered(
        &self,
        url: &str,
    ) -> Result<(Arc<BufferedMediaSource>, BufferWriter), String> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| e.to_string())?;
        let mut response = client
            .get(url)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await
            .map_err(|e| crate::remote_stream::describe_reqwest_error(&e))?;
        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()));
        }
        let (source, writer) =
            self.streaming_buffer(StreamingConfig::fast_start(), response.content_length())?;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| crate::remote_stream::describe_reqwest_error(&e))?
        {
            writer.push_chunk(&chunk)?;
        }
        writer.complete()?;
        Ok((Arc::new(source), writer))
    }
}

pub(super) fn source_metadata(
    source: &BufferedMediaSource,
) -> Result<(AudioMetadata, u64), String> {
    let stream = MediaSourceStream::new(Box::new(source.create_reader()), Default::default());
    let format = get_probe()
        .format(
            &Hint::new(),
            stream,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|error| format!("Cached audio probe failed: {error}"))?;
    let track = format
        .format
        .default_track()
        .ok_or("No supported track in cached file")?;
    let meta = audio_metadata_from_codec_params(&track.codec_params)?;
    let duration = track.codec_params.n_frames.unwrap_or(0) / u64::from(meta.sample_rate.max(1));
    Ok((meta, duration))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_prefetch_releases_the_fetch_marker() {
        let cache = Arc::new(qbz_cache::AudioCache::new(1024));
        let entered = Arc::new(tokio::sync::Notify::new());
        let task_cache = cache.clone();
        let task_entered = entered.clone();
        let task = tokio::spawn(async move {
            let _guard = PrefetchGuard::new(task_cache, 42);
            task_entered.notify_one();
            std::future::pending::<()>().await;
        });
        entered.notified().await;
        assert!(cache.is_fetching(42));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!cache.is_fetching(42));
    }
}
