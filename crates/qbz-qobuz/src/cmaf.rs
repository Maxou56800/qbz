//! CMAF streaming pipeline for Qobuz.
//!
//! Qobuz's modern mobile client uses CMAF (Common Media Application Format)
//! segmented streaming over Akamai CDN, with AES-CTR per-frame encryption.
//! This is the pipeline that the v9.7.0.3 Android app uses and the one we
//! need to match if we want to stay compatible as Qobuz deprecates the
//! legacy `/track/getFileUrl` nginx path.
//!
//! # Pipeline shape
//!
//! 1. `/file/url` returns `{ url_template, key (wrapped), n_segments, ... }`
//! 2. `/session/start` returns `{ session_id, infos }` — the `infos` string
//!    is the HKDF salt needed to derive the per-session AES key
//! 3. Session key = `HKDF(CMAF_SEED, infos)`
//! 4. Content key = unwrap(session_key, key) — this is the per-track AES key
//! 5. Fetch init segment (s=0) → parse FLAC header + segment table
//! 6. For each s=1..n_segments: fetch → parse crypto boxes → decrypt frames
//!    in place → emit decrypted FLAC frames to the consumer
//!
//! # Why live in `qbz-qobuz` and not `qbz-cmaf`
//!
//! `qbz-cmaf` is pure parsing + crypto primitives (no I/O, no Qobuz client).
//! This module is the Qobuz-specific orchestration: it calls `/file/url`,
//! `/session/start`, owns the Akamai HTTP client, and returns ready-to-play
//! or ready-to-store bundles.
//!
//! # Why two variants
//!
//! - [`download_full`] — returns the fully decrypted FLAC as `Vec<u8>`. Used
//!   by the playback pipeline for in-memory cache writes and eager downloads.
//! - [`download_raw`] — returns a [`CmafRawBundle`] of **encrypted** segments
//!   plus key material. Used by the offline cache so we can persist
//!   bit-identical bytes to what Qobuz delivered, and decrypt only at
//!   playback time. This is the security-sensitive path.

use std::sync::Arc;
use std::time::Duration;

use qbz_models::{Quality, StreamQualityInfo};

use crate::client::QobuzClient;
use crate::error::Result;

/// Concurrency cap for the full-download path. 3 segments in flight is the
/// empirically-determined sweet spot — Akamai CDN rate-limits with 1s windows
/// past ~5 parallel requests per client IP.
pub const CMAF_PREFETCH_CONCURRENCY: usize = 3;

/// Total deadline for one CMAF CDN fetch attempt, from connect through the
/// complete response body. Qobuz audio segments are independently retryable;
/// leaving a single body read unbounded can strand both the live feeder and
/// the gapless successor fetch long after the audible buffer has run dry.
///
/// The retry layer makes up to three attempts, so this is deliberately an
/// attempt deadline rather than a whole-track deadline.
pub const CMAF_FETCH_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(15);

/// Progress callback shape for the download helpers. Each call reports
/// "k of n segments complete" with the bytes received for that segment,
/// so the caller can emit UI progress events without knowing the CMAF
/// internals. Callbacks must be `Send + Sync` because segments are
/// fetched in parallel.
pub type CmafProgressCallback =
    std::sync::Arc<dyn Fn(CmafProgressUpdate) + Send + Sync>;

/// A single progress tick. `segments_completed` is cumulative (1..=n),
/// `n_segments` is the total including the init segment if you count it.
#[derive(Debug, Clone, Copy)]
pub struct CmafProgressUpdate {
    pub segments_completed: u32,
    pub n_segments: u32,
    pub bytes_this_segment: u64,
}

/// Info gathered from the CMAF init segment, enough to start streaming
/// playback. The caller is expected to fetch audio segments 1..n_segments
/// and feed them through [`qbz_cmaf::parse_segment_crypto`] +
/// [`qbz_cmaf::decrypt_frame`].
pub struct CmafStreamingInfo {
    pub url_template: String,
    pub n_segments: u8,
    pub content_key: [u8; 16],
    pub flac_header: Vec<u8>,
    pub segment_table: Vec<qbz_cmaf::SegmentTableEntry>,
    pub format_id: u32,
    pub sampling_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    /// How long the init segment fetch took (ms), for speed estimation.
    pub init_fetch_ms: u64,
}

/// Raw (encrypted) CMAF bundle suitable for offline storage.
///
/// Everything in this struct is **bit-identical** to what Qobuz's CDN
/// returned. In particular:
///
/// - `init_bytes` is the raw init segment (unencrypted mp4 box with the
///   FLAC header inside — cheap to store).
/// - `segments` are the raw encrypted segment mp4 files, one per
///   `s=1..=n_segments`. These are useless without `content_key` and
///   without running them through the CMAF decrypt pipeline.
/// - `content_key` is the 16-byte AES key unwrapped from the session key;
///   it must be stored **encrypted at rest** on the caller's side.
/// - `infos` is the original `session/start` infos string. With the
///   `CMAF_SEED` constant this is enough to re-derive `session_key` and
///   re-unwrap the content key if we ever need to audit or migrate.
///
/// The intent is that an attacker who copies the user's offline directory
/// out without also extracting the OS-keyring wrapped `content_key` gets
/// nothing usable — the segments are encrypted, the `infos` is just a
/// salt, and the seed alone isn't enough.
pub struct CmafRawBundle {
    pub init_bytes: Vec<u8>,
    pub segments: Vec<Vec<u8>>,
    pub content_key: [u8; 16],
    pub infos: String,
    pub format_id: u32,
    pub sampling_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub n_segments: u8,
}

/// Prepare CMAF streaming: fetch init segment only, derive keys, return info.
/// Does NOT download audio segments -- the caller streams those in background.
pub async fn setup_streaming(
    client: &QobuzClient,
    track_id: u64,
    quality: Quality,
) -> std::result::Result<CmafStreamingInfo, String> {
    let file_url = client.get_file_url(track_id, quality).await
        .map_err(|e| format!("get_file_url failed: {}", e))?;

    let url_template = file_url
        .url_template
        .as_ref()
        .ok_or("No url_template in file/url response")?
        .clone();
    let key_str = file_url
        .key
        .as_ref()
        .ok_or("No key in file/url response")?;

    let (_session_id, infos) = client.ensure_cmaf_session().await
        .map_err(|e| format!("ensure_cmaf_session failed: {}", e))?;

    let session_key = qbz_cmaf::derive_session_key(crate::auth::CMAF_SEED, &infos)
        .map_err(|e| format!("Session key derivation failed: {}", e))?;
    let content_key = qbz_cmaf::unwrap_content_key(&session_key, key_str)
        .map_err(|e| format!("Content key unwrap failed: {}", e))?;

    // Fetch only the init segment (s=0) -- typically small, <500ms
    let http = build_cdn_client()?;
    let init_url = url_template.replace("$SEGMENT$", "0");
    let init_start = std::time::Instant::now();

    log::info!("[CMAF] Fetching init segment for track {}", track_id);
    let init_data = fetch_cdn_bytes_with_retry(&http, &init_url, "CMAF init")
        .await
        .map_err(|e| format!("Failed to fetch init segment: {}", e))?;

    let init_fetch_ms = init_start.elapsed().as_millis() as u64;

    let init_info = qbz_cmaf::parse_init_segment(&init_data)
        .map_err(|e| format!("Failed to parse init segment: {}", e))?;

    log::info!(
        "[CMAF] Init for track {}: FLAC header {}B, segment_table={} entries, API n_segments={}, fetched in {}ms",
        track_id,
        init_info.flac_header.len(),
        init_info.segment_table.len(),
        file_url.n_segments,
        init_fetch_ms
    );
    if init_info.segment_table.len() != file_url.n_segments as usize {
        log::warn!(
            "[CMAF] MISMATCH for track {}: segment_table has {} entries but API says n_segments={}",
            track_id,
            init_info.segment_table.len(),
            file_url.n_segments
        );
    }

    let format_id = file_url.format_id.unwrap_or(quality.id());

    Ok(CmafStreamingInfo {
        url_template,
        n_segments: file_url.n_segments,
        content_key,
        flac_header: init_info.flac_header,
        segment_table: init_info.segment_table,
        format_id,
        sampling_rate: file_url.sampling_rate,
        bit_depth: file_url.bits_depth.or(file_url.bit_depth),
        init_fetch_ms,
    })
}

/// Download a track's complete CMAF stream and return decrypted FLAC bytes.
///
/// Used by the playback path for in-memory cache writes. Segments are
/// fetched concurrently with a semaphore cap, decrypted, and concatenated.
pub async fn download_full(
    client: &QobuzClient,
    track_id: u64,
    quality: Quality,
) -> std::result::Result<Vec<u8>, String> {
    download_full_with_progress(client, track_id, quality, None).await
}

/// Same as [`download_full`] but with a progress callback fired once per
/// completed segment.
pub async fn download_full_with_progress(
    client: &QobuzClient,
    track_id: u64,
    quality: Quality,
    on_progress: Option<CmafProgressCallback>,
) -> std::result::Result<Vec<u8>, String> {
    download_full_with_quality_progress(client, track_id, quality, on_progress)
        .await
        .map(|(bytes, _quality)| bytes)
}

/// Like [`download_full`] but also returns the quality actually resolved from
/// the CMAF init segment (`format_id` / `sampling_rate` / `bit_depth`). Used
/// by the external-stream (Cast / DLNA) path, which must surface the real
/// delivered quality. The CMAF path always yields decrypted FLAC, so the
/// caller's content type is `audio/flac`.
pub async fn download_full_with_quality(
    client: &QobuzClient,
    track_id: u64,
    quality: Quality,
) -> std::result::Result<(Vec<u8>, StreamQualityInfo), String> {
    download_full_with_quality_progress(client, track_id, quality, None).await
}

/// [`download_full_with_quality`] + a per-segment progress callback.
pub async fn download_full_with_quality_progress(
    client: &QobuzClient,
    track_id: u64,
    quality: Quality,
    on_progress: Option<CmafProgressCallback>,
) -> std::result::Result<(Vec<u8>, StreamQualityInfo), String> {
    let setup = setup_streaming(client, track_id, quality).await?;
    let http = build_cdn_client()?;

    let output = assemble_download(&http, &setup, on_progress).await?;
    log::info!(
        "[CMAF-FULL] Track {} complete: {:.2} MB FLAC (segment lengths verified)",
        track_id,
        output.len() as f64 / (1024.0 * 1024.0),
    );

    // `from_raw` normalizes the rate unit (kHz vs Hz) defensively.
    let quality_info = StreamQualityInfo::from_raw(
        setup.format_id,
        setup.sampling_rate.map(|v| v as f64),
        setup.bit_depth,
    );
    Ok((output, quality_info))
}

/// Hold at most three encrypted segments while assembling in source order.
/// Futures belong to this stream: cancellation/error drops outstanding HTTP
/// work, and a slow leading segment cannot accumulate the rest of the track.
async fn assemble_download(
    http: &reqwest::Client, setup: &CmafStreamingInfo,
    on_progress: Option<CmafProgressCallback>,
) -> std::result::Result<Vec<u8>, String> {
    use futures_util::{stream, StreamExt};
    if setup.segment_table.len() != usize::from(setup.n_segments) {
        return Err("CMAF segment count disagrees with init table".into());
    }
    let expected_size = setup.segment_table.iter().try_fold(setup.flac_header.len(), |total, entry| {
        total.checked_add(entry.byte_len as usize).ok_or("CMAF output size overflow")
    })?;
    let mut output = Vec::new();
    output.try_reserve_exact(expected_size).map_err(|_| "CMAF output allocation failed")?;
    output.extend_from_slice(&setup.flac_header);
    let mut segments = stream::iter(1..=setup.n_segments).map(|index| async move {
        let url = setup.url_template.replace("$SEGMENT$", &index.to_string());
        let data = fetch_cdn_bytes_with_retry(http, &url, "CMAF-FULL").await?;
        // Preserve the existing CDN pacing per occupied request slot.
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok::<_, String>((index, data))
    }).buffered(CMAF_PREFETCH_CONCURRENCY);
    while let Some(segment) = segments.next().await {
        let (index, data) = segment?;
        append_verified_segment(&data, &setup.content_key,
            setup.segment_table[usize::from(index) - 1].byte_len as usize, &mut output)?;
        if let Some(progress) = &on_progress {
            progress(CmafProgressUpdate { segments_completed: u32::from(index),
                n_segments: u32::from(setup.n_segments), bytes_this_segment: data.len() as u64 });
        }
    }
    if output.len() != expected_size { return Err("CMAF assembled size mismatch".into()); }
    Ok(output)
}

fn append_verified_segment(data: &[u8], key: &[u8; 16], expected: usize, output: &mut Vec<u8>)
    -> std::result::Result<(), String> {
    let crypto = qbz_cmaf::parse_segment_crypto(data).map_err(|e| e.to_string())?;
    let size = crypto.mdat_end.checked_sub(crypto.data_offset).ok_or("CMAF invalid media bounds")?;
    if crypto.mdat_end > data.len() || size != expected {
        return Err("CMAF segment size disagrees with init table".into());
    }
    let mut cursor = crypto.data_offset;
    for entry in &crypto.entries {
        cursor = cursor.checked_add(entry.size as usize).filter(|end| *end <= crypto.mdat_end)
            .ok_or("CMAF frame exceeds media bounds")?;
    }
    // All bounds have been checked before the first mutation. Append and
    // decrypt directly in the final allocation; no second decoded segment.
    cursor = crypto.data_offset;
    for entry in &crypto.entries {
        let end = cursor + entry.size as usize;
        let start = output.len();
        output.extend_from_slice(&data[cursor..end]);
        if entry.flags != 0 { qbz_cmaf::decrypt_frame(key, &entry.iv, &mut output[start..]); }
        cursor = end;
    }
    output.extend_from_slice(&data[cursor..crypto.mdat_end]);
    Ok(())
}

/// Download a track's complete CMAF stream and return it as a raw (still
/// encrypted) bundle suitable for offline storage.
///
/// The caller is responsible for:
/// 1. Persisting `init_bytes` + `segments` to disk as bit-identical blobs
/// 2. Wrapping `content_key` with a device-bound key before storing it
/// 3. Storing `infos` (either wrapped or as plaintext — it's only a salt,
///    useless without `CMAF_SEED` + `content_key`)
///
/// At playback time, the caller feeds `init_bytes` through
/// [`qbz_cmaf::parse_init_segment`] to recover the FLAC header + segment
/// table, then decrypts each segment with the unwrapped content key.
pub async fn download_raw(
    client: &QobuzClient,
    track_id: u64,
    quality: Quality,
) -> std::result::Result<CmafRawBundle, String> {
    download_raw_with_progress(client, track_id, quality, None).await
}

/// Same as [`download_raw`] but with a progress callback fired once per
/// completed audio segment. The init segment doesn't count toward progress
/// — it's downloaded up front and is typically tiny (<1% of total bytes).
pub async fn download_raw_with_progress(
    client: &QobuzClient,
    track_id: u64,
    quality: Quality,
    on_progress: Option<CmafProgressCallback>,
) -> std::result::Result<CmafRawBundle, String> {
    let file_url = client.get_file_url(track_id, quality).await
        .map_err(|e| format!("get_file_url failed: {}", e))?;

    let url_template = file_url
        .url_template
        .as_ref()
        .ok_or("No url_template in file/url response")?
        .clone();
    let key_str = file_url
        .key
        .as_ref()
        .ok_or("No key in file/url response")?;

    let (_session_id, infos) = client.ensure_cmaf_session().await
        .map_err(|e| format!("ensure_cmaf_session failed: {}", e))?;

    let session_key = qbz_cmaf::derive_session_key(crate::auth::CMAF_SEED, &infos)
        .map_err(|e| format!("Session key derivation failed: {}", e))?;
    let content_key = qbz_cmaf::unwrap_content_key(&session_key, key_str)
        .map_err(|e| format!("Content key unwrap failed: {}", e))?;

    let http = build_cdn_client()?;

    // Init segment — used for FLAC header + segment table at playback
    let init_url = url_template.replace("$SEGMENT$", "0");
    log::info!("[CMAF-RAW] Fetching init for track {}", track_id);
    let init_bytes = http
        .get(&init_url)
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch init segment: {}", e))?
        .bytes()
        .await
        .map_err(|e| format!("Failed to read init segment: {}", e))?
        .to_vec();

    // Audio segments — encrypted, stored as-is
    let segments = fetch_all_segments(
        &http,
        &url_template,
        file_url.n_segments,
        "CMAF-RAW",
        on_progress,
    )
    .await?;

    log::info!(
        "[CMAF-RAW] Track {} bundle: init={}B, {} encrypted segments, total raw size={} bytes",
        track_id,
        init_bytes.len(),
        segments.len(),
        init_bytes.len() + segments.iter().map(|s| s.len()).sum::<usize>(),
    );

    Ok(CmafRawBundle {
        init_bytes,
        segments,
        content_key,
        infos,
        format_id: file_url.format_id.unwrap_or(quality.id()),
        sampling_rate: file_url.sampling_rate,
        bit_depth: file_url.bits_depth.or(file_url.bit_depth),
        n_segments: file_url.n_segments,
    })
}

/// Build a reqwest client configured for Akamai CDN fetches.
///
/// Uses the workspace reqwest feature set (rustls-tls). The original in-tree
/// version in `src-tauri/commands_v2/helpers.rs` called `.use_native_tls()`
/// but the src-tauri Cargo opts into both stacks; this crate stays on
/// rustls for smaller binary + no system SSL dependency. If Akamai ever
/// surfaces a cert issue, adding the `native-tls` feature to qbz-qobuz is
/// the escape hatch.
fn build_cdn_client() -> std::result::Result<reqwest::Client, String> {
    qbz_net_proxy::apply_current(reqwest::Client::builder())
        .map_err(|e| format!("CMAF client proxy error: {}", e))?
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("CMAF client error: {}", e))
}

/// Fetch a CDN URL into bytes, retrying transient failures (network blips,
/// 5xx, 429) with exponential backoff. A terminal status (404/403) fails
/// immediately. Without this a single transient segment failure aborted the
/// whole track download and the frontend skipped it — issue #467.
pub async fn fetch_cdn_bytes_with_retry(
    http: &reqwest::Client,
    url: &str,
    log_tag: &str,
) -> std::result::Result<Vec<u8>, String> {
    fetch_cdn_bytes_with_retry_config(
        http,
        url,
        log_tag,
        CMAF_FETCH_ATTEMPT_TIMEOUT,
        crate::retry::DEFAULT_MAX_ATTEMPTS,
    )
    .await
}

async fn fetch_cdn_bytes_with_retry_config(
    http: &reqwest::Client,
    url: &str,
    log_tag: &str,
    attempt_timeout: Duration,
    max_attempts: u32,
) -> std::result::Result<Vec<u8>, String> {
    use crate::retry::{classify_reqwest, classify_status, retry_transient, FetchError};
    retry_transient(
        max_attempts,
        log_tag,
        FetchError::is_transient,
        |_attempt| async move {
            // TIMING SPLIT (2026-08-10): the owner measured this same fetch at
            // 423ms under the Slint binary and 5459ms under Qt — same shared
            // crate, same machine, same network, and (verified with
            // `cargo tree -e features`) the same unified reqwest feature set.
            // `send()` covers DNS + connect + TLS + time-to-headers; `bytes()`
            // is the body. Splitting them says which half is losing the five
            // seconds instead of a sixth guess.
            let t_send = std::time::Instant::now();
            let response = http
                .get(url)
                .header("User-Agent", "Mozilla/5.0")
                // reqwest applies this from the start of connect until the
                // response body completes. The deadline is recreated on each
                // retry, so one stalled segment cannot monopolize a feeder.
                .timeout(attempt_timeout)
                .send()
                .await
                .map_err(|e| classify_reqwest(&e, "fetch"))?;
            let send_ms = t_send.elapsed().as_millis();
            let status = response.status();
            if !status.is_success() {
                return Err(classify_status(status, "fetch"));
            }
            let t_body = std::time::Instant::now();
            let out = response
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| classify_reqwest(&e, "read"));
            log::info!(
                "[{}] timing: send(dns+connect+tls+ttfb)={}ms body={}ms",
                log_tag,
                send_ms,
                t_body.elapsed().as_millis()
            );
            out
        },
    )
    .await
    .map_err(|e| e.to_string())
}

/// Fetch segments 1..=n_segments concurrently with a semaphore cap and a
/// cooldown per slot to stay under CDN rate limits.
///
/// If `on_progress` is `Some`, it's invoked once per completed segment
/// (not per HTTP chunk — the cooldown happens on the worker, not here).
/// Callbacks fire in completion order, not segment order.
async fn fetch_all_segments(
    http: &reqwest::Client,
    url_template: &str,
    n_segments: u8,
    log_tag: &str,
    on_progress: Option<CmafProgressCallback>,
) -> std::result::Result<Vec<Vec<u8>>, String> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(CMAF_PREFETCH_CONCURRENCY));
    let seg_indices: Vec<u8> = (1..=n_segments).collect();
    // JoinSet owns every segment task: dropping this fetch (for example when
    // engine-empty recovery advances the queue) aborts the children too.
    // Plain JoinHandles detach on drop and would keep the abandoned gapless
    // download competing with the replacement track.
    let mut fetches = tokio::task::JoinSet::new();

    let completed_count = Arc::new(std::sync::atomic::AtomicU32::new(0));

    for seg_idx in seg_indices {
        let sem = semaphore.clone();
        let http = http.clone();
        let seg_url = url_template.replace("$SEGMENT$", &seg_idx.to_string());
        let log_tag = log_tag.to_string();
        let progress = on_progress.clone();
        let counter = completed_count.clone();

        fetches.spawn(async move {
            let permit = sem.acquire_owned().await.map_err(|e| format!("semaphore: {}", e))?;
            let seg_data = fetch_cdn_bytes_with_retry(
                &http,
                &seg_url,
                &format!("{} seg {}", log_tag, seg_idx),
            )
            .await
            .map_err(|e| format!("[{}] seg {} fetch: {}", log_tag, seg_idx, e))?;
            let bytes_this_segment = seg_data.len() as u64;
            if let Some(cb) = progress {
                let done = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                cb(CmafProgressUpdate {
                    segments_completed: done,
                    n_segments: n_segments as u32,
                    bytes_this_segment,
                });
            }
            // Cooldown before releasing the slot — keeps requests spaced out
            // to stay under CDN rate limits (most use 1s windows)
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            drop(permit);
            Ok::<(u8, Vec<u8>), String>((seg_idx, seg_data))
        });
    }

    // Collect results in arrival order, then re-sort by segment index
    let mut segments: Vec<(u8, Vec<u8>)> = Vec::with_capacity(n_segments as usize);
    while let Some(result) = fetches.join_next().await {
        let (idx, data) = result
            .map_err(|e| format!("[{}] task panic: {}", log_tag, e))?
            .map_err(|e| format!("[{}] download failed: {}", log_tag, e))?;
        segments.push((idx, data));
    }
    segments.sort_by_key(|(idx, _)| *idx);
    Ok(segments.into_iter().map(|(_, data)| data).collect())
}

/// Decrypt a sequence of encrypted CMAF segments in order and append the
/// decrypted frames to `output`.
///
/// This is the common decryption logic shared between the full-download
/// path (decrypt-then-return) and the offline playback path (decrypt-from-
/// disk-then-feed-player).
///
/// Hot-path note: the previous implementation allocated a `Vec<u8>` per
/// frame, copied the encrypted bytes into it, decrypted in place, then
/// copied again into `output` via `extend_from_slice`. For a HiRes FLAC
/// this is tens of thousands of small heap allocations + double copies
/// per track. Now we extend `output` with the encrypted bytes directly
/// and decrypt the just-appended slice in place — one copy instead of
/// three, zero per-frame allocations. Combined with AES-NI codegen
/// (enabled via `target-cpu=x86-64-v3` in `.cargo/config.toml`) this
/// is the difference between a 20-second offline-cache gap on track
/// transitions and a sub-second one.
pub fn decrypt_segments_into(
    segments: &[Vec<u8>],
    content_key: &[u8; 16],
    output: &mut Vec<u8>,
) -> std::result::Result<(), String> {
    for (seg_idx, seg_data) in segments.iter().enumerate() {
        // seg_idx is 0-based here but the original segment number is idx+1
        let log_idx = seg_idx + 1;
        let crypto = qbz_cmaf::parse_segment_crypto(seg_data)
            .map_err(|e| format!("CMAF seg {} parse: {}", log_idx, e))?;

        let mut data_pos = crypto.data_offset;
        for entry in &crypto.entries {
            let frame_end = data_pos + entry.size as usize;
            if frame_end > seg_data.len() {
                return Err(format!("CMAF seg {} frame overflow", log_idx));
            }
            let output_start = output.len();
            output.extend_from_slice(&seg_data[data_pos..frame_end]);
            if entry.flags != 0 {
                qbz_cmaf::decrypt_frame(content_key, &entry.iv, &mut output[output_start..]);
            }
            data_pos = frame_end;
        }
        if data_pos < crypto.mdat_end && crypto.mdat_end <= seg_data.len() {
            output.extend_from_slice(&seg_data[data_pos..crypto.mdat_end]);
        }
    }
    Ok(())
}

// Silence "unused imports" if we end up not using everything at some point;
// the Result alias is kept for future variants that want to surface ApiError.
#[allow(dead_code)]
fn _type_assertions() {
    let _: fn() -> Result<()> = || Ok(());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Once};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn segment_fixture(bytes: &[u8], frame_size: u32) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&44u32.to_be_bytes());
        data.extend_from_slice(b"uuid");
        data.extend_from_slice(&[0x3b,0x42,0x12,0x92,0x56,0xf3,0x5f,0x75,0x92,0x36,0x63,0xb6,0x9a,0x1f,0x52,0xb2]);
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&52u32.to_be_bytes());
        data.extend_from_slice(&[0, 0, 0, 1]); // no IV, one clear frame
        data.extend_from_slice(&frame_size.to_be_bytes());
        data.extend_from_slice(&[0; 4]); // skip, flags
        data.extend_from_slice(&(8u32 + bytes.len() as u32).to_be_bytes());
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(bytes);
        data
    }

    #[test]
    fn incremental_assembly_preserves_frames_and_clear_tail() {
        let segment = segment_fixture(b"frame-tail", 5);
        let mut output = b"fLaC".to_vec();
        append_verified_segment(&segment, &[0; 16], 10, &mut output).unwrap();
        assert_eq!(output, b"fLaCframe-tail");
        for (data, expected) in [(segment.clone(), 9), (segment_fixture(b"frame", 6), 5),
            (segment[..segment.len()-1].to_vec(), 10)] {
            let mut output = b"fLaC".to_vec();
            assert!(append_verified_segment(&data, &[0; 16], expected, &mut output).is_err());
            assert_eq!(output, b"fLaC");
        }
    }

    #[tokio::test]
    async fn incremental_download_bounds_out_of_order_requests_and_preserves_order() {
        install_tls_provider();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requested = Arc::new(AtomicU32::new(0));
        let observed = requested.clone();
        let release_first = Arc::new(tokio::sync::Notify::new());
        let release = release_first.clone();
        let server = tokio::spawn(async move {
            let mut requests = tokio::task::JoinSet::new();
            for _ in 0..6 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let release = release.clone();
                let observed = observed.clone();
                requests.spawn(async move {
                    let mut request = [0; 1024];
                    let len = socket.read(&mut request).await.unwrap();
                    let request = std::str::from_utf8(&request[..len]).unwrap();
                    let index: u8 = request.split_whitespace().nth(1).unwrap()[1..].parse().unwrap();
                    observed.fetch_add(1, Ordering::SeqCst);
                    if index == 1 { release.notified().await; }
                    let body = segment_fixture(&[index], 1);
                    let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    socket.write_all(header.as_bytes()).await.unwrap();
                    socket.write_all(&body).await.unwrap();
                });
            }
            while let Some(result) = requests.join_next().await { result.unwrap(); }
        });
        let download = tokio::spawn(async move {
            let setup = CmafStreamingInfo {
                url_template: format!("http://{address}/$SEGMENT$"), n_segments: 6,
                content_key: [0; 16], flac_header: b"fLaC".to_vec(),
                segment_table: vec![qbz_cmaf::SegmentTableEntry { byte_len: 1, sample_count: 1 }; 6],
                format_id: 27, sampling_rate: Some(96_000), bit_depth: Some(24), init_fetch_ms: 0,
            };
            assemble_download(&build_cdn_client().unwrap(), &setup, None).await.unwrap()
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while requested.load(Ordering::SeqCst) < 3 { tokio::task::yield_now().await; }
        }).await.unwrap();
        // The following two requests have completed, but the leading request
        // still owns its slot: no unbounded task/segment accumulation.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(requested.load(Ordering::SeqCst), 3);
        release_first.notify_one();
        let bytes = tokio::time::timeout(Duration::from_secs(4), download).await.unwrap().unwrap();
        assert_eq!(bytes, b"fLaC\x01\x02\x03\x04\x05\x06");
        server.await.unwrap();
    }

    fn install_tls_provider() {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }

    #[tokio::test]
    async fn stalled_segment_body_times_out_and_retries() {
        install_tls_provider();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let accepts = Arc::new(AtomicU32::new(0));
        let server_accepts = accepts.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                server_accepts.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut request = [0u8; 1024];
                    let _ = socket.read(&mut request).await;
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                    // Headers arrive immediately; the body never does within
                    // the attempt deadline. This reproduces the 52 s body stall
                    // from the 2026-08-28 session without a live CDN.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                });
            }
        });

        let client = reqwest::Client::builder().build().unwrap();
        let url = format!("http://{address}/segment");
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            fetch_cdn_bytes_with_retry_config(
                &client,
                &url,
                "CMAF timeout test",
                Duration::from_millis(50),
                2,
            ),
        )
        .await
        .expect("bounded segment attempts must not hang");

        let error = result.expect_err("both stalled attempts must time out");
        assert!(
            error.contains("read:"),
            "stalled response must fail while reading its body: {error}"
        );
        assert_eq!(accepts.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn aborting_full_fetch_cancels_its_segment_tasks() {
        install_tls_provider();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (headers_tx, headers_rx) = tokio::sync::oneshot::channel();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            let _ = headers_tx.send(());

            let mut trailing_request = [0u8; 1024];
            while socket.read(&mut trailing_request).await.unwrap_or(0) != 0 {
                // Drain any request bytes left after the server's first read.
            }
            let _ = closed_tx.send(());
        });

        let client = reqwest::Client::builder().build().unwrap();
        let url_template = format!("http://{address}/segment-$SEGMENT$");
        let fetch = tokio::spawn(async move {
            fetch_all_segments(&client, &url_template, 1, "CMAF abort test", None).await
        });
        tokio::time::timeout(Duration::from_secs(1), headers_rx)
            .await
            .expect("segment request must reach the body stall")
            .unwrap();

        fetch.abort();
        let _ = fetch.await;
        tokio::time::timeout(Duration::from_secs(1), closed_rx)
            .await
            .expect("aborting the owner must close its in-flight segment")
            .unwrap();
        server.await.unwrap();
    }
}
