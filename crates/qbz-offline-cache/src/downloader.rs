//! Stream fetcher for caching tracks to disk

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::event::{CacheEvent, CacheEventSink, CacheFormat};
use crate::types::OfflineCacheStatus;

/// Maximum number of download retry attempts
const MAX_RETRIES: u32 = 3;

/// Backoff durations for each retry attempt
const RETRY_BACKOFFS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(5),
];

/// StreamFetcher handles fetching audio streams and caching them to disk.
///
/// Creates a fresh HTTP client per download to avoid HTTP/2 connection pool
/// poisoning: when a CDN connection breaks mid-transfer, a persistent client's
/// pool can keep reusing the dead connection, causing all subsequent downloads
/// to fail after 1 byte. Ephemeral clients guarantee a clean connection pool.
pub struct StreamFetcher;

impl StreamFetcher {
    pub fn new() -> Self {
        Self
    }

    /// Build a fresh reqwest::Client for a single download.
    ///
    /// Each download gets its own client to prevent HTTP/2 connection pool
    /// poisoning from affecting subsequent downloads.
    fn build_client() -> Result<reqwest::Client, String> {
        // rustls via the workspace reqwest (same TLS backend qbz-qobuz uses
        // for all CDN traffic, including CMAF segment downloads).
        qbz_net_proxy::apply_current(reqwest::Client::builder())
            .map_err(|e| format!("Failed to apply proxy configuration: {}", e))?
            .timeout(Duration::from_secs(300)) // 5 minute timeout for large files
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))
    }

    /// Fetch a stream and cache it to disk with progress updates.
    ///
    /// Retries up to MAX_RETRIES times with exponential backoff on transient
    /// failures (connection reset, EOF, timeout). Each retry creates a fresh
    /// HTTP client to avoid reusing a poisoned connection pool.
    pub async fn fetch_to_file(
        &self,
        url: &str,
        dest_path: &Path,
        track_id: u64,
        sink: Option<&CacheEventSink>,
    ) -> Result<u64, String> {
        log::info!("Caching track {} to {:?}", track_id, dest_path);

        // Create parent directories if needed
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        }

        let temp_path = dest_path.with_extension("tmp");

        let mut last_error = String::new();
        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let backoff = RETRY_BACKOFFS[(attempt - 1) as usize];
                log::info!(
                    "[Offline] Retry {}/{} for track {} after {}s",
                    attempt,
                    MAX_RETRIES,
                    track_id,
                    backoff.as_secs()
                );
                tokio::time::sleep(backoff).await;
            }

            // Fresh client per attempt — prevents connection pool poisoning
            let client = Self::build_client()?;

            match self
                .try_download(&client, url, &temp_path, track_id, sink)
                .await
            {
                Ok(size) => {
                    // Move temp file to final destination
                    std::fs::rename(&temp_path, dest_path)
                        .map_err(|e| format!("Failed to move temp file: {}", e))?;
                    log::info!("Caching complete for track {}: {} bytes", track_id, size);
                    return Ok(size);
                }
                Err(e) => {
                    last_error = e;
                    // Clean up partial temp file before retry
                    let _ = std::fs::remove_file(&temp_path);
                    if attempt < MAX_RETRIES {
                        log::warn!(
                            "[Offline] Download attempt {} failed for track {}: {}",
                            attempt + 1,
                            track_id,
                            last_error
                        );
                    }
                }
            }
        }

        Err(last_error)
    }

    /// Single download attempt: stream response body to a temp file.
    async fn try_download(
        &self,
        client: &reqwest::Client,
        url: &str,
        temp_path: &Path,
        track_id: u64,
        sink: Option<&CacheEventSink>,
    ) -> Result<u64, String> {
        let response = client
            .get(url)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await
            .map_err(|e| format!("Failed to start fetch: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()));
        }

        let total_size = response.content_length();
        log::info!(
            "Caching started for track {}, total size: {:?} bytes",
            track_id,
            total_size
        );

        let mut file = std::fs::File::create(temp_path)
            .map_err(|e| format!("Failed to create temp file: {}", e))?;

        let mut cached: u64 = 0;
        let mut last_progress: u8 = 0;
        let mut last_emit_time = Instant::now();
        const MIN_EMIT_INTERVAL: Duration = Duration::from_millis(200);

        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| {
                use std::error::Error as _;
                let mut msg = format!("Fetch error: {}", e);
                let mut source = e.source();
                while let Some(cause) = source {
                    msg.push_str(&format!(" | caused by: {}", cause));
                    source = cause.source();
                }
                log::error!(
                    "[Offline] Download error for track {} after {} bytes: {}",
                    track_id,
                    cached,
                    msg
                );
                msg
            })?;

            file.write_all(&chunk)
                .map_err(|e| format!("Failed to write chunk: {}", e))?;

            cached += chunk.len() as u64;

            // Calculate progress
            let progress = if let Some(total) = total_size {
                ((cached as f64 / total as f64) * 100.0) as u8
            } else {
                0
            };

            // Emit progress event every 2% change AND at least 200ms apart (always emit 100%)
            let elapsed = last_emit_time.elapsed();
            if progress != last_progress
                && (progress - last_progress >= 2 || progress == 100)
                && (elapsed >= MIN_EMIT_INTERVAL || progress == 100)
            {
                last_progress = progress;
                last_emit_time = Instant::now();

                if let Some(sink) = sink {
                    sink(CacheEvent::Progress {
                        track_id,
                        progress_percent: progress,
                        bytes_downloaded: cached,
                        total_bytes: total_size,
                    });
                }

                log::debug!(
                    "Caching progress for track {}: {}% ({}/{:?} bytes)",
                    track_id,
                    progress,
                    cached,
                    total_size
                );
            }
        }

        // Ensure all data is written
        file.flush()
            .map_err(|e| format!("Failed to flush file: {}", e))?;
        drop(file);

        validate_download_size(track_id, cached, total_size)?;

        Ok(cached)
    }

    /// Fetch to memory (for smaller files or streaming)
    pub async fn fetch_to_memory(&self, url: &str) -> Result<Vec<u8>, String> {
        let client = Self::build_client()?;

        let response = client
            .get(url)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await
            .map_err(|e| format!("Failed to fetch: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("Failed to read bytes: {}", e))?;

        Ok(bytes.to_vec())
    }
}

impl Default for StreamFetcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared helper: spawn the download task for a single track.
/// CMAF-first offline download path (v2 format).
///
/// On success: the encrypted CMAF bundle is persisted under
/// `<offline_root>/tracks-cmaf/<track_id>/`, the per-track AES content key
/// + session infos are wrapped via `qbz-secrets` and stored on the DB row,
/// `cache_format` flips to 2, `mark_complete` fires, and the library row
/// is populated with the same metadata the legacy path would populate
/// (title/artist/album, etc.).
///
/// Returns `Err` for any failure that makes CMAF unusable — the caller
/// falls back to the legacy plain-FLAC path.
pub(crate) async fn try_cmaf_offline_download(
    track_id: u64,
    db: &std::sync::Arc<tokio::sync::Mutex<Option<crate::OfflineCacheDb>>>,
    offline_root: &str,
    library_db: &std::sync::Arc<tokio::sync::Mutex<Option<qbz_library::LibraryDatabase>>>,
    client: &std::sync::Arc<tokio::sync::RwLock<Option<qbz_qobuz::QobuzClient>>>,
    sink: &CacheEventSink,
) -> Result<(), String> {
    use qbz_models::Quality;

    let offline_root_path = std::path::PathBuf::from(offline_root);

    // Progress callback: emit the same `offline:caching_progress` event
    // shape the legacy StreamFetcher fires, so the UI's progress ring
    // doesn't care whether the bytes came from CMAF or legacy.
    //
    // Note: one 'started' event here up front so the frontend sees
    // 'downloading' status immediately (the ring starts empty); actual
    // percentage updates arrive per completed segment.
    sink(CacheEvent::Progress {
        track_id,
        progress_percent: 0,
        bytes_downloaded: 0,
        total_bytes: None,
    });

    let sink_for_cb = sink.clone();
    let progress_cb: qbz_qobuz::CmafProgressCallback = std::sync::Arc::new(
        move |update: qbz_qobuz::CmafProgressUpdate| {
            let percent = if update.n_segments > 0 {
                (update.segments_completed as f64 / update.n_segments as f64 * 100.0)
                    .round()
                    .clamp(0.0, 100.0) as u8
            } else {
                0u8
            };
            sink_for_cb(CacheEvent::Progress {
                track_id,
                progress_percent: percent,
                bytes_downloaded: update.bytes_this_segment,
                total_bytes: None,
            });
        },
    );

    // Fetch the raw CMAF bundle. Requires an initialized QobuzClient; if
    // it is missing, bail so the legacy path runs.
    let bundle = {
        let client_guard = client.read().await;
        let qobuz_client = client_guard
            .as_ref()
            .ok_or_else(|| "QobuzClient not initialized".to_string())?;
        qbz_qobuz::cmaf::download_raw_with_progress(
            qobuz_client,
            track_id,
            Quality::UltraHiRes,
            Some(progress_cb),
        )
        .await?
    };

    // Open the secret vault and wrap the keying material on a BLOCKING thread.
    // The OS keyring (secret-service) does a synchronous D-Bus round-trip via
    // zbus, which PANICS ("cannot start a runtime from within a runtime") when
    // run on an async worker. spawn_blocking moves it off the async pool — the
    // same reason the playback decrypt path already uses spawn_blocking.
    let offline_root_for_vault = offline_root_path.clone();
    let content_key = bundle.content_key;
    let infos = bundle.infos.clone();
    let (content_key_wrapped, infos_wrapped) = tokio::task::spawn_blocking(move || {
        let vault = crate::secret_vault::get_or_init(&offline_root_for_vault)
            .map_err(|e| format!("SecretBox init failed: {}", e))?;
        let ck = vault
            .wrap(&content_key)
            .map_err(|e| format!("Failed to wrap content_key: {}", e))?;
        let inf = vault
            .wrap(infos.as_bytes())
            .map_err(|e| format!("Failed to wrap infos: {}", e))?;
        Ok::<(Vec<u8>, Vec<u8>), String>((ck, inf))
    })
    .await
    .map_err(|e| format!("vault task join failed: {}", e))??;

    // Persist the encrypted bundle to disk.
    let (layout, total_bytes) =
        crate::cmaf_store::persist_bundle(&offline_root_path, track_id, &bundle)?;

    // Flip the DB row to v2 and store the wrapped keying material.
    {
        let db_guard = db.lock().await;
        let db_ref = db_guard
            .as_ref()
            .ok_or_else(|| "Offline cache DB not open".to_string())?;
        db_ref.set_cmaf_bundle(
            track_id,
            layout.segments_path.to_string_lossy().as_ref(),
            layout.init_path.to_string_lossy().as_ref(),
            &content_key_wrapped,
            &infos_wrapped,
            bundle.format_id,
            bundle.n_segments as u32,
            total_bytes,
        )?;
        db_ref
            .mark_complete(track_id, total_bytes)
            .map_err(|e| format!("Failed to mark_complete: {}", e))?;
    }

    // Fetch metadata for the library row (same source the legacy path uses).
    // We don't write FLAC tags or embed artwork INSIDE the encrypted blob
    // — that would corrupt it. But we DO save a cover.jpg next to the
    // bundle directory so the library UI has artwork to display.
    let metadata = {
        let client_guard = client.read().await;
        match client_guard.as_ref() {
            Some(qc) => crate::metadata::fetch_complete_metadata(track_id, qc).await,
            None => Err("QobuzClient not initialized".to_string()),
        }
    };
    if let Ok(metadata) = metadata {
        // Download and save album artwork alongside the bundle, same as
        // the legacy path does next to the FLAC file. cover.jpg lives at
        // <offline_root>/tracks-cmaf/<track_id>/cover.jpg — set as the
        // library row's artwork_path so the UI picks it up.
        let artwork_path: Option<String> = if let Some(artwork_url) = metadata.artwork_url.as_deref() {
            match crate::metadata::save_album_artwork(&layout.track_dir, artwork_url).await {
                Ok(()) => {
                    let cover = layout.track_dir.join("cover.jpg");
                    if cover.exists() {
                        Some(cover.to_string_lossy().to_string())
                    } else {
                        None
                    }
                }
                Err(e) => {
                    log::warn!("[Offline/CMAF] Track {} artwork save failed: {}", track_id, e);
                    None
                }
            }
        } else {
            None
        };

        let album_artist = metadata.album_artist.as_ref().unwrap_or(&metadata.artist);
        let album_group_key = format!("{}|{}", metadata.album, album_artist);
        let lib_opt = library_db.lock().await;
        if let Some(lib_guard) = lib_opt.as_ref() {
            let result = lib_guard.insert_qobuz_cached_track_with_grouping(
                track_id,
                &metadata.title,
                &metadata.artist,
                Some(&metadata.album),
                metadata.album_artist.as_deref(),
                metadata.track_number,
                metadata.disc_number,
                metadata.year,
                metadata.duration_secs,
                // For v2 bundles the "playable path" in the library index
                // is the track directory; the player resolves it through
                // the DB's cache_format=2 branch anyway.
                layout.track_dir.to_string_lossy().as_ref(),
                &album_group_key,
                &metadata.album,
                bundle.bit_depth,
                bundle.sampling_rate.map(|r| r as f64),
                artwork_path.as_deref(),
            );
            if let Err(error) = result {
                log::warn!("[offline-library] track={track_id} metadata insert failed: {error}");
            }
        }
    } else if let Err(e) = metadata {
        log::warn!(
            "[Offline/CMAF] Track {} post-metadata fetch failed: {} (bundle already persisted)",
            track_id,
            e
        );
    }

    log::info!(
        "[Offline/CMAF] Track {} cached as v2 bundle: {:.2} MB under {:?}",
        track_id,
        total_bytes as f64 / (1024.0 * 1024.0),
        layout.track_dir
    );
    sink(CacheEvent::Completed {
        track_id,
        size: total_bytes,
        format: CacheFormat::Cmaf,
    });
    sink(CacheEvent::Processed {
        track_id,
        path: layout.track_dir.to_string_lossy().to_string(),
        format: CacheFormat::Cmaf,
    });
    Ok(())
}

/// Used by both v2_cache_track_for_offline (single) and v2_cache_tracks_batch_for_offline (batch).
#[allow(clippy::too_many_arguments)]
pub fn spawn_track_cache_download(
    track_id: u64,
    file_path: std::path::PathBuf,
    client: std::sync::Arc<tokio::sync::RwLock<Option<qbz_qobuz::QobuzClient>>>,
    fetcher: std::sync::Arc<crate::StreamFetcher>,
    db: std::sync::Arc<tokio::sync::Mutex<Option<crate::OfflineCacheDb>>>,
    offline_root: String,
    library_db: std::sync::Arc<tokio::sync::Mutex<Option<qbz_library::LibraryDatabase>>>,
    sink: CacheEventSink,
    semaphore: std::sync::Arc<tokio::sync::Semaphore>,
) {
    tokio::spawn(async move {
        let _permit = match semaphore.acquire_owned().await {
            Ok(permit) => permit,
            Err(err) => {
                log::error!(
                    "Failed to acquire cache slot for track {}: {}",
                    track_id,
                    err
                );
                if let Some(db_guard) = db.lock().await.as_ref() {
                    let _ = db_guard.update_status(
                        track_id,
                        crate::OfflineCacheStatus::Failed,
                        Some("Failed to start caching"),
                    );
                }
                sink(CacheEvent::Failed {
                    track_id,
                    error: "Failed to acquire cache slot".to_string(),
                });
                return;
            }
        };

        if let Some(db_guard) = db.lock().await.as_ref() {
            let _ = db_guard.update_status(
                track_id,
                crate::OfflineCacheStatus::Downloading,
                None,
            );
        }
        sink(CacheEvent::Started { track_id });

        // === CMAF-first offline download (v2 format) ===
        //
        // Stores bit-identical encrypted segments + wrapped content key.
        // Falls through to the legacy path below if any step fails (no
        // CoreBridge yet, /file/url returns a non-CMAF response, network
        // flake, vault init failure, etc.). The legacy fallback keeps
        // existing users unblocked while we validate the new path.
        match try_cmaf_offline_download(
            track_id,
            &db,
            &offline_root,
            &library_db,
            &client,
            &sink,
        )
        .await
        {
            Ok(()) => return,
            Err(e) => {
                log::warn!(
                    "[Offline/CMAF] Track {} — CMAF path failed ({}), falling back to legacy /track/getFileUrl",
                    track_id,
                    e
                );
            }
        }

        let stream_url = {
            let client_guard = client.read().await;
            match client_guard.as_ref() {
                Some(qc) => qc
                    .get_stream_url_with_fallback(track_id, qbz_models::Quality::UltraHiRes)
                    .await
                    .map_err(|e| e.to_string()),
                None => Err("QobuzClient not initialized".to_string()),
            }
        };

        let url = match stream_url {
            Ok(s) => s.url,
            Err(e) => {
                log::error!("Failed to get stream URL for track {}: {}", track_id, e);
                if let Some(db_guard) = db.lock().await.as_ref() {
                    let _ = db_guard.update_status(
                        track_id,
                        OfflineCacheStatus::Failed,
                        Some(&format!("Failed to get stream URL: {}", e)),
                    );
                }
                sink(CacheEvent::Failed { track_id, error: e });
                return;
            }
        };

        match fetcher
            .fetch_to_file(&url, &file_path, track_id, Some(&sink))
            .await
        {
            Ok(size) => {
                log::info!("Caching complete for track {}: {} bytes", track_id, size);
                if let Some(db_guard) = db.lock().await.as_ref() {
                    let _ = db_guard.mark_complete(track_id, size);
                }
                sink(CacheEvent::Completed {
                    track_id,
                    size,
                    format: CacheFormat::Flac,
                });

                // Post-processing kept inline to avoid command->command delegation.
                let file_path_str = file_path.to_string_lossy().to_string();
                let metadata = {
                    let client_guard = client.read().await;
                    let result = match client_guard.as_ref() {
                        Some(qc) => {
                            crate::metadata::fetch_complete_metadata(track_id, qc).await
                        }
                        None => Err("QobuzClient not initialized".to_string()),
                    };
                    match result {
                        Ok(m) => m,
                        Err(e) => {
                            log::warn!(
                                "Post-processing metadata fetch failed for {}: {}",
                                track_id,
                                e
                            );
                            return;
                        }
                    }
                };

                if let Err(e) =
                    crate::metadata::write_flac_tags(&file_path_str, &metadata)
                {
                    log::warn!("Failed to write tags for {}: {}", track_id, e);
                }
                if let Some(artwork_url) = &metadata.artwork_url {
                    if let Err(e) =
                        crate::metadata::embed_artwork(&file_path_str, artwork_url)
                            .await
                    {
                        log::warn!("Failed to embed artwork for {}: {}", track_id, e);
                    }
                }

                let new_path = match crate::metadata::organize_cached_file(
                    track_id,
                    &file_path_str,
                    &offline_root,
                    &metadata,
                ) {
                    Ok(p) => p,
                    Err(e) => {
                        log::warn!("Failed to organize cached file {}: {}", track_id, e);
                        return;
                    }
                };

                // Save cover.jpg next to the organized FLAC so the library
                // UI has artwork to display.
                let artwork_path_v1: Option<String> =
                    if let Some(artwork_url) = metadata.artwork_url.as_deref() {
                        if let Some(parent_dir) = std::path::Path::new(&new_path).parent() {
                            match crate::metadata::save_album_artwork(
                                parent_dir,
                                artwork_url,
                            )
                            .await
                            {
                                Ok(()) => {
                                    let cover = parent_dir.join("cover.jpg");
                                    if cover.exists() {
                                        Some(cover.to_string_lossy().to_string())
                                    } else {
                                        None
                                    }
                                }
                                Err(_) => None,
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                let (bit_depth_detected, sample_rate_detected) =
                    match lofty::read_from_path(&new_path) {
                        Ok(tagged_file) => {
                            use lofty::prelude::*;
                            let properties = tagged_file.properties();
                            (
                                properties.bit_depth().map(|bd| bd as u32),
                                properties.sample_rate().map(|sr| sr as f64),
                            )
                        }
                        Err(_) => (None, None),
                    };

                let album_artist = metadata.album_artist.as_ref().unwrap_or(&metadata.artist);
                let album_group_key = format!("{}|{}", metadata.album, album_artist);
                let lib_opt = library_db.lock().await;
                if let Some(lib_guard) = lib_opt.as_ref() {
                    let result = lib_guard.insert_qobuz_cached_track_with_grouping(
                        track_id,
                        &metadata.title,
                        &metadata.artist,
                        Some(&metadata.album),
                        metadata.album_artist.as_deref(),
                        metadata.track_number,
                        metadata.disc_number,
                        metadata.year,
                        metadata.duration_secs,
                        &new_path,
                        &album_group_key,
                        &metadata.album,
                        bit_depth_detected,
                        sample_rate_detected,
                        artwork_path_v1.as_deref(),
                    );
                    if let Err(error) = result {
                        log::warn!(
                            "[offline-library] track={track_id} metadata insert failed: {error}"
                        );
                    }
                }

                if let Some(db_guard) = db.lock().await.as_ref() {
                    let _ = db_guard.update_file_path(track_id, &new_path);
                }

                sink(CacheEvent::Processed {
                    track_id,
                    path: new_path,
                    format: CacheFormat::Flac,
                });
            }
            Err(e) => {
                log::error!("Caching failed for track {}: {}", track_id, e);
                if let Some(db_guard) = db.lock().await.as_ref() {
                    let _ = db_guard.update_status(
                        track_id,
                        OfflineCacheStatus::Failed,
                        Some(&e),
                    );
                }
                sink(CacheEvent::Failed { track_id, error: e });
            }
        }
    });
}


/// Validate downloaded byte count against optional Content-Length.
/// Fail closed when length is known and mismatched, or when zero bytes.
pub(crate) fn validate_download_size(
    track_id: u64,
    cached: u64,
    content_length: Option<u64>,
) -> Result<(), String> {
    if let Some(expected) = content_length {
        if cached != expected {
            return Err(format!(
                "Truncated download for track {track_id}: got {cached} bytes, Content-Length was {expected}"
            ));
        }
    }
    if cached == 0 {
        return Err(format!("Empty download for track {track_id}"));
    }
    Ok(())
}

#[cfg(test)]
mod download_size_tests {
    use super::validate_download_size;

    #[test]
    fn accepts_matching_length() {
        assert!(validate_download_size(1, 100, Some(100)).is_ok());
    }

    #[test]
    fn rejects_truncated() {
        let err = validate_download_size(7, 50, Some(100)).unwrap_err();
        assert!(err.contains("Truncated"));
        assert!(err.contains("50"));
        assert!(err.contains("100"));
    }

    #[test]
    fn rejects_empty_even_without_length() {
        assert!(validate_download_size(2, 0, None).is_err());
    }

    #[test]
    fn accepts_nonzero_unknown_length() {
        assert!(validate_download_size(3, 42, None).is_ok());
    }
}
