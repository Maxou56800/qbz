// TODO(converge: qconnect-glue) — copied from crates/qbz/src/remote_stream.rs @ b4450965;
// do not fix bugs here without fixing the source, and vice versa.
//! Shared HTTP streaming feeder.
//!
//! Ports the Tauri `track_loading.rs` progressive feeder verbatim: probe a
//! remote audio URL for size + FLAC format, open the player's progressive
//! streaming sink (`Player::play_streaming_dynamic`), then push the body to the
//! returned `BufferWriter` chunk-by-chunk as it arrives. Playback starts as soon
//! as the initial buffer fills — not after the whole file lands.
//!
//! `reqwest + BufferWriter` bound only, so it stays frontend-side and never
//! crosses the qconnect-app boundary. Used by BOTH the QConnect renderer
//! (`qconnect_engine.rs`) and the Plex playback path (`playback.rs`), so there
//! is exactly one feeder.
//!
//! BIT-PERFECT: `play_streaming_dynamic` decodes the same original bytes and
//! drives the PROTECTED device init from the decoded stream. The
//! `sample_rate`/`bit_depth` parsed here are only the streaming-config hints;
//! the audio backend (`pipewire_backend.rs`, `init_device`, `audio_settings.rs`)
//! is untouched.

use std::time::Duration;

use qbz_player::{BufferWriter, Player};

/// Owned background feeder for one progressive remote stream.
///
/// Tokio normally detaches a task when its `JoinHandle` is dropped. This
/// wrapper deliberately aborts instead: the renderer engine owns exactly one
/// feeder, so replacing/stopping an authority cannot leave an old CDN request
/// writing into a retired buffer.
#[must_use = "dropping the feeder aborts the background CDN request"]
pub struct RemoteStreamFeeder {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl RemoteStreamFeeder {
    fn new(task: tokio::task::JoinHandle<()>) -> Self {
        Self { task: Some(task) }
    }
}

impl Drop for RemoteStreamFeeder {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Format/size facts sniffed from a remote audio URL before streaming.
pub struct RemoteStreamInfo {
    pub content_length: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub bit_depth: u32,
    pub speed_mbps: f64,
}

/// Probe + open the progressive sink + spawn the background feeder.
///
/// On success the player has begun buffering and `play_streaming_dynamic` will
/// start audio once the initial buffer fills; the body download runs in a
/// spawned task. Errors here mean the caller should fall back to a full
/// download (the probe or the sink open failed).
pub async fn stream_remote_track_into_player(
    player: &Player,
    track_id: u64,
    duration_secs: u64,
    start_position_secs: u64,
    playing: bool,
    url: &str,
    log_tag: &str,
    authority_check: impl FnOnce() -> Result<(), String>,
) -> Result<RemoteStreamFeeder, String> {
    let stream_info = probe_remote_stream_info(url).await?;
    authority_check()?;
    log::info!(
        "[{}/STREAMING] Track {} - {:.2} MB, {}Hz, {} ch, {}-bit, {:.1} MB/s",
        log_tag,
        track_id,
        stream_info.content_length as f64 / (1024.0 * 1024.0),
        stream_info.sample_rate,
        stream_info.channels,
        stream_info.bit_depth,
        stream_info.speed_mbps
    );

    let writer = player
        .play_streaming_dynamic_with_state(
            track_id,
            stream_info.sample_rate,
            stream_info.channels,
            stream_info.bit_depth,
            stream_info.content_length,
            stream_info.speed_mbps,
            duration_secs,
            start_position_secs,
            playing,
        )
        .map_err(|err| format!("start streaming remote track {track_id}: {err}"))?;

    let url = url.to_string();
    let content_length = stream_info.content_length;
    let log_tag = log_tag.to_string();
    let task = tokio::spawn(async move {
        if let Err(err) =
            download_and_stream_remote_track(&url, writer, track_id, content_length, &log_tag).await
        {
            log::error!(
                "[{}/STREAMING] Track {} failed while streaming: {}",
                log_tag,
                track_id,
                err
            );
        }
    });

    Ok(RemoteStreamFeeder::new(task))
}

/// HEAD for content-length, then a small `Range: bytes=0-65535` GET to (a)
/// measure throughput and (b) parse the FLAC `STREAMINFO` block for the real
/// sample rate / channels / bit depth. Never defaults silently for FLAC (a
/// wrong sample rate would silently resample hi-res).
pub async fn probe_remote_stream_info(url: &str) -> Result<RemoteStreamInfo, String> {
    use std::time::Instant;

    let client = qbz_net_proxy::apply_current(reqwest::Client::builder())
        .map_err(|_| "apply proxy configuration failed".to_string())?
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "create stream probe HTTP client failed".to_string())?;

    let head_response = client
        .head(url)
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await
        .map_err(|err| {
            format!(
                "probe HEAD request failed: {}",
                describe_reqwest_error(&err)
            )
        })?;

    if !head_response.status().is_success() {
        return Err(format!(
            "probe HEAD request failed with status {}",
            head_response.status()
        ));
    }

    let content_length = head_response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| "probe missing content-length header".to_string())?;

    let start_time = Instant::now();
    let range_response = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0")
        .header("Range", "bytes=0-65535")
        .send()
        .await
        .map_err(|err| {
            format!(
                "probe range request failed: {}",
                describe_reqwest_error(&err)
            )
        })?;

    if !range_response.status().is_success() {
        return Err(format!(
            "probe range request failed with status {}",
            range_response.status()
        ));
    }

    let initial_bytes = range_response
        .bytes()
        .await
        .map_err(|err| format!("read probe bytes failed: {}", describe_reqwest_error(&err)))?;

    let elapsed = start_time.elapsed();
    let speed_mbps = if elapsed.as_secs_f64() > 0.0 {
        (initial_bytes.len() as f64 / elapsed.as_secs_f64()) / (1024.0 * 1024.0)
    } else {
        10.0
    };

    let (sample_rate, channels, bit_depth) =
        if initial_bytes.len() >= 26 && initial_bytes.starts_with(b"fLaC") {
            let sample_rate = ((initial_bytes[18] as u32) << 12)
                | ((initial_bytes[19] as u32) << 4)
                | ((initial_bytes[20] as u32) >> 4);
            let channels = ((initial_bytes[20] >> 1) & 0x07) + 1;
            let bit_depth = ((initial_bytes[20] & 0x01) << 4) | ((initial_bytes[21] >> 4) & 0x0F);
            (sample_rate, channels as u16, (bit_depth + 1) as u32)
        } else {
            log::warn!("[remote-stream] Non-FLAC probe for remote handoff, using defaults");
            (44_100, 2, 16)
        };

    Ok(RemoteStreamInfo {
        content_length,
        sample_rate,
        channels,
        bit_depth,
        speed_mbps,
    })
}

/// Plain full-body GET → `bytes_stream()` loop → `writer.push_chunk` →
/// `writer.complete()`. No HTTP Range on the main GET (the `BufferedMediaSource`
/// buffers every pushed byte and serves seeks from the growing buffer).
pub async fn download_and_stream_remote_track(
    url: &str,
    writer: BufferWriter,
    track_id: u64,
    content_length: u64,
    log_tag: &str,
) -> Result<(), String> {
    use futures_util::StreamExt;
    use std::time::Instant;

    struct FailGuard {
        writer: BufferWriter,
        armed: bool,
    }
    impl Drop for FailGuard {
        fn drop(&mut self) {
            if self.armed {
                let _ = self
                    .writer
                    .error("remote stream aborted before completion".into());
            }
        }
    }
    let mut guard = FailGuard {
        writer,
        armed: true,
    };
    let writer = &guard.writer;

    let client = qbz_net_proxy::apply_current(reqwest::Client::builder())
        .map_err(|_| "apply proxy configuration failed".to_string())?
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|_| "create remote streaming HTTP client failed".to_string())?;

    let response = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await
        .map_err(|err| {
            format!(
                "start remote streaming request failed: {}",
                describe_reqwest_error(&err)
            )
        })?;

    if !response.status().is_success() {
        return Err(format!(
            "remote streaming request failed with status {}",
            response.status()
        ));
    }

    let mut bytes_received = 0u64;
    let mut stream = response.bytes_stream();
    let start_time = Instant::now();
    let mut last_log_time = Instant::now();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|err| {
            format!(
                "remote streaming chunk failed: {}",
                describe_reqwest_error(&err)
            )
        })?;
        bytes_received += chunk.len() as u64;

        if let Err(err) = writer.push_chunk(&chunk) {
            log::error!(
                "[{}/STREAMING] Failed to push chunk for track {}: {}",
                log_tag,
                track_id,
                err
            );
            guard.armed = false;
            let _ = writer.error(format!("push_chunk failed: {err}"));
            return Err(format!("push_chunk failed: {err}"));
        }

        let now = Instant::now();
        if now.duration_since(last_log_time) >= Duration::from_secs(2) && content_length > 0 {
            let progress = (bytes_received as f64 / content_length as f64) * 100.0;
            let avg_speed =
                (bytes_received as f64 / start_time.elapsed().as_secs_f64()) / (1024.0 * 1024.0);
            log::info!(
                "[{}/STREAMING] Track {} {:.1}% ({:.2}/{:.2} MB) @ {:.2} MB/s",
                log_tag,
                track_id,
                progress,
                bytes_received as f64 / (1024.0 * 1024.0),
                content_length as f64 / (1024.0 * 1024.0),
                avg_speed
            );
            last_log_time = now;
        }
    }

    guard.armed = false;
    if let Err(err) = writer.complete() {
        log::error!(
            "[{}/STREAMING] Failed to mark stream complete for track {}: {}",
            log_tag,
            track_id,
            err
        );
        let _ = writer.error(format!("complete failed: {err}"));
        return Err(format!("complete failed: {err}"));
    }

    log::info!(
        "[{}/STREAMING] Track {} complete: {:.2} MB in {:.1}s",
        log_tag,
        track_id,
        bytes_received as f64 / (1024.0 * 1024.0),
        start_time.elapsed().as_secs_f64()
    );

    Ok(())
}

/// Return a bounded, URL-free diagnostic for a reqwest failure.
///
/// The raw error and its source chain can contain the signed CDN URL (including
/// its query). We inspect the chain only to preserve the known header-limit
/// classification and never copy an arbitrary cause into the returned string.
pub fn describe_reqwest_error(err: &reqwest::Error) -> String {
    if error_chain_has_header_limit(err) {
        return safe_transport_diagnostic(
            "message head is too large; signed URL and query are intentionally omitted",
        )
        .to_string();
    }

    if err.is_timeout() {
        "HTTP transport timed out".to_string()
    } else if err.is_connect() {
        "HTTP transport connection failed".to_string()
    } else if err.is_body() {
        "HTTP response body failed".to_string()
    } else if err.is_decode() {
        "HTTP response decode failed".to_string()
    } else if err.is_status() {
        "HTTP status rejected".to_string()
    } else {
        "HTTP transport request failed".to_string()
    }
}

fn error_chain_has_header_limit(err: &reqwest::Error) -> bool {
    use std::error::Error as _;

    if is_header_flood_error(&err.to_string()) {
        return true;
    }
    let mut source = err.source();
    while let Some(cause) = source {
        if is_header_flood_error(&cause.to_string()) {
            return true;
        }
        source = cause.source();
    }
    false
}

/// Map arbitrary transport text to one of a few constant diagnostics. This is
/// intentionally lossy: CDN URLs and their credential-bearing query strings
/// are never valid log or API output.
fn safe_transport_diagnostic(message: &str) -> &'static str {
    if is_header_flood_error(message) {
        "HTTP response header limit exceeded (message head is too large)"
    } else {
        "HTTP transport request failed"
    }
}

/// True when a sanitized diagnostic (or an internal raw cause inspected before
/// logging) shows hyper's hard-coded h1 100-header cap.
/// Akamai answers SMALL raw-url objects with ~106 headers (the `X-AK-GRN` /
/// `X-AK-FWD-ERROR: ERR_POC_FWD_OBJ_TOO_SMALL` flood), so EVERY reqwest fetch
/// of such an URL fails this way — streaming probe and full download alike.
pub fn is_header_flood_error(message: &str) -> bool {
    let haystack = message.to_ascii_lowercase();
    haystack.contains("message head is too large") || haystack.contains("too many headers")
}

#[cfg(test)]
mod tests {
    use super::{is_header_flood_error, safe_transport_diagnostic};

    #[test]
    fn cdn_diagnostic_never_echoes_url_or_query() {
        let raw = "request failed for https://cdn.example/audio.flac?jwt=super-secret";
        let diagnostic = safe_transport_diagnostic(raw);
        assert_eq!(diagnostic, "HTTP transport request failed");
        assert!(!diagnostic.contains("cdn.example"));
        assert!(!diagnostic.contains("super-secret"));
    }

    #[test]
    fn sanitized_header_limit_remains_machine_classifiable() {
        let raw = "https://cdn.example/audio?token=secret: message head is too large";
        let diagnostic = safe_transport_diagnostic(raw);
        assert!(is_header_flood_error(diagnostic));
        assert!(!diagnostic.contains("cdn.example"));
        assert!(!diagnostic.contains("secret"));
    }
}
