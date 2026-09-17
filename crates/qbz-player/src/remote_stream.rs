//! Progressive Range-streaming feeder — SHARED.
//!
//! Lifted verbatim out of `crates/qbz/src/remote_stream.rs` (the Slint binary),
//! where it had lived for no structural reason: its only imports are `std` and
//! this crate's own `BufferWriter` / `Player`, i.e. it never depended on Slint
//! at all. The Qt port had no streaming feeder because of that placement, so
//! its Plex path downloaded the WHOLE file before the first note — 58 MB, and
//! measurably slower than a Qobuz track streamed off the internet.
//!
//! ADR-006 (frontend-agnostic core) is the rule this restores.
//!
//! This shared extraction is now the sole implementation used by the
//! application frontends and renderer paths.

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

use crate::{BufferWriter, Player};

/// Format/size facts sniffed from a remote audio URL before streaming.
pub struct RemoteStreamInfo {
    pub content_length: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub bit_depth: u32,
    /// Container/codec family detected from the bytes actually served.
    pub format: &'static str,
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
    url: &str,
    log_tag: &str,
) -> Result<(), String> {
    stream_remote_track_into_player_with_headers(
        player,
        track_id,
        duration_secs,
        start_position_secs,
        url,
        log_tag,
        &[],
    )
    .await
}

/// Variant of [`stream_remote_track_into_player`] that preserves a source's
/// HTTP request contract while keeping this transport source-agnostic.
///
/// Plex media parts are the motivating case: their token is in the URL, but
/// some servers still reject the Range/body request unless the same
/// `X-Plex-*` identity used for metadata is present. Header values are never
/// included in errors or logs.
pub async fn stream_remote_track_into_player_with_headers(
    player: &Player,
    track_id: u64,
    duration_secs: u64,
    start_position_secs: u64,
    url: &str,
    log_tag: &str,
    request_headers: &[(String, String)],
) -> Result<(), String> {
    let stream_info = probe_remote_stream_info_with_headers(url, request_headers).await?;
    log::info!(
        "[{}/STREAMING] Track {} - {:.2} MB, {}Hz, {} ch, {}-bit {}, {:.1} MB/s",
        log_tag,
        track_id,
        stream_info.content_length as f64 / (1024.0 * 1024.0),
        stream_info.sample_rate,
        stream_info.channels,
        stream_info.bit_depth,
        stream_info.format,
        stream_info.speed_mbps
    );

    let writer = player
        .play_streaming_dynamic(
            track_id,
            stream_info.sample_rate,
            stream_info.channels,
            stream_info.bit_depth,
            stream_info.content_length,
            stream_info.speed_mbps,
            duration_secs,
            start_position_secs,
        )
        .map_err(|err| format!("start streaming remote track {track_id}: {err}"))?;

    let url = url.to_string();
    let content_length = stream_info.content_length;
    let log_tag = log_tag.to_string();
    let request_headers = request_headers.to_vec();
    tokio::spawn(async move {
        if let Err(err) = download_and_stream_remote_track_with_headers(
            &url,
            writer,
            track_id,
            content_length,
            &log_tag,
            &request_headers,
        )
        .await
        {
            log::error!(
                "[{}/STREAMING] Track {} failed while streaming: {}",
                log_tag,
                track_id,
                err
            );
        }
    });

    Ok(())
}

/// One small `Range: bytes=0-65535` GET supplies content length, throughput and
/// the audio header. The old HEAD + Range sequence paid two serial round trips
/// before the real body request; on Plex that was visible on every MP3 start,
/// and a surprising number of otherwise-valid media servers reject HEAD.
///
/// A server that ignores Range may answer 200 with the whole file. We consume
/// only the first 64 KiB from its byte stream and drop the response, rather
/// than downloading the track once as a "probe" and then a second time into
/// the player.
pub async fn probe_remote_stream_info(url: &str) -> Result<RemoteStreamInfo, String> {
    probe_remote_stream_info_with_headers(url, &[]).await
}

pub async fn probe_remote_stream_info_with_headers(
    url: &str,
    request_headers: &[(String, String)],
) -> Result<RemoteStreamInfo, String> {
    use futures_util::StreamExt;
    use reqwest::header::{ACCEPT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE};
    use reqwest::StatusCode;
    use std::time::Instant;

    let client = qbz_net_proxy::apply_current(reqwest::Client::builder())
        .map_err(|err| format!("apply proxy configuration: {err}"))?
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| format!("create stream probe client: {err}"))?;

    let start_time = Instant::now();
    let request = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0")
        .header(ACCEPT_ENCODING, "identity")
        .header("Range", "bytes=0-65535");
    let range_response = apply_request_headers(request, request_headers)?
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

    let partial = range_response.status() == StatusCode::PARTIAL_CONTENT;
    let content_range = range_response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok());
    let response_length = range_response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok());
    let content_length = probe_content_length(partial, content_range, response_length)
        .ok_or_else(|| "probe missing content length/range header".to_string())?;

    const PROBE_BYTES: usize = 65_536;
    let mut initial_bytes = Vec::with_capacity(PROBE_BYTES);
    let mut stream = range_response.bytes_stream();
    while initial_bytes.len() < PROBE_BYTES {
        let Some(chunk) = stream.next().await else {
            break;
        };
        let chunk = chunk
            .map_err(|err| format!("read probe bytes failed: {}", describe_reqwest_error(&err)))?;
        let take = (PROBE_BYTES - initial_bytes.len()).min(chunk.len());
        initial_bytes.extend_from_slice(&chunk[..take]);
    }
    if initial_bytes.is_empty() {
        return Err("probe returned no audio bytes".to_string());
    }

    let elapsed = start_time.elapsed();
    let speed_mbps = if elapsed.as_secs_f64() > 0.0 {
        (initial_bytes.len() as f64 / elapsed.as_secs_f64()) / (1024.0 * 1024.0)
    } else {
        10.0
    };

    // FLAC's fixed STREAMINFO parser remains the strongest answer. For MP3,
    // AAC, ALAC and the other direct-play containers, ask the same Symphonia
    // probe the player itself uses. This removes the old unconditional
    // 44.1/16 guess (wrong for 48 kHz MP3) before the protected backend opens.
    let (sample_rate, channels, bit_depth, format) =
        match qbz_models::probe_streaminfo(&initial_bytes) {
            Some(p) => (p.sample_rate, p.channels, p.bits_per_sample, "FLAC"),
            None => match crate::player::extract_audio_metadata_full(&initial_bytes) {
                Ok(meta) => {
                    let format = if meta.codec == symphonia::core::codecs::CODEC_TYPE_MP3 {
                        "MP3"
                    } else {
                        "AUDIO"
                    };
                    (
                        meta.sample_rate,
                        meta.channels,
                        meta.bit_depth.unwrap_or(16),
                        format,
                    )
                }
                Err(err) => {
                    log::warn!(
                        "[remote-stream] Header probe failed ({err}); using conservative 44.1/16"
                    );
                    (44_100, 2, 16, "AUDIO")
                }
            },
        };

    Ok(RemoteStreamInfo {
        content_length,
        sample_rate,
        channels,
        bit_depth,
        format,
        speed_mbps,
    })
}

/// Parse the total from RFC 7233's `Content-Range`, e.g.
/// `bytes 0-65535/44790678`. `*` means the server does not know the total.
fn content_range_total(value: &str) -> Option<u64> {
    value.rsplit_once('/')?.1.trim().parse().ok()
}

/// A 206 `Content-Length` describes only the returned slice, never the media
/// object. Accept it as the total only for a 200 response where the server
/// ignored Range and sent the complete representation.
fn probe_content_length(
    partial: bool,
    content_range: Option<&str>,
    content_length: Option<&str>,
) -> Option<u64> {
    content_range.and_then(content_range_total).or_else(|| {
        if partial {
            None
        } else {
            content_length?.parse().ok()
        }
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
    download_and_stream_remote_track_with_headers(
        url,
        writer,
        track_id,
        content_length,
        log_tag,
        &[],
    )
    .await
}

pub async fn download_and_stream_remote_track_with_headers(
    url: &str,
    writer: BufferWriter,
    track_id: u64,
    content_length: u64,
    log_tag: &str,
    request_headers: &[(String, String)],
) -> Result<(), String> {
    use futures_util::StreamExt;
    use reqwest::header::ACCEPT_ENCODING;
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
        .map_err(|err| format!("apply proxy configuration: {err}"))?
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|err| format!("create remote streaming client: {err}"))?;

    let request = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0")
        .header(ACCEPT_ENCODING, "identity");
    let response = apply_request_headers(request, request_headers)?
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

fn apply_request_headers(
    request: reqwest::RequestBuilder,
    headers: &[(String, String)],
) -> Result<reqwest::RequestBuilder, String> {
    Ok(request.headers(parsed_request_headers(headers)?))
}

fn parsed_request_headers(
    headers: &[(String, String)],
) -> Result<reqwest::header::HeaderMap, String> {
    use reqwest::header::{HeaderName, HeaderValue};

    let mut parsed = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("invalid remote request header name {name:?}"))?;
        let header_value = HeaderValue::from_str(value)
            .map_err(|_| format!("invalid value for remote request header {name:?}"))?;
        parsed.insert(header_name, header_value);
    }
    Ok(parsed)
}

pub use qbz_qobuz::net_diag::{describe_reqwest_error, is_header_flood_error};

#[cfg(test)]
mod tests {
    use super::{content_range_total, parsed_request_headers, probe_content_length};

    #[test]
    fn source_request_headers_are_parsed_for_the_http_request() {
        let headers =
            parsed_request_headers(&[("X-Plex-Product".to_string(), "QBZ".to_string())]).unwrap();
        assert_eq!(
            headers
                .get("X-Plex-Product")
                .and_then(|value| value.to_str().ok()),
            Some("QBZ")
        );
    }

    #[test]
    fn content_range_uses_the_whole_object_length() {
        assert_eq!(
            content_range_total("bytes 0-65535/44790678"),
            Some(44_790_678)
        );
        assert_eq!(content_range_total("bytes 0-65535/*"), None);
    }

    #[test]
    fn partial_content_length_is_never_mistaken_for_the_track_size() {
        assert_eq!(
            probe_content_length(true, Some("bytes 0-65535/44790678"), Some("65536")),
            Some(44_790_678)
        );
        assert_eq!(probe_content_length(true, None, Some("65536")), None);
    }

    #[test]
    fn full_response_length_is_valid_when_range_is_ignored() {
        assert_eq!(
            probe_content_length(false, None, Some("44790678")),
            Some(44_790_678)
        );
    }
}
