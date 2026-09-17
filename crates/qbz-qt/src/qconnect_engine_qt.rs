//! Qobuz Connect renderer engine for the Qt frontend (block B2 of the
//! 2026-08-01 QConnect Qt-port contract).
//!
//! Implements [`qconnect_app::QconnectRendererEngine`] over the Qt
//! `AppRuntime`'s `QbzCore` + `Player`, so qbz-qt becomes a QConnect renderer
//! that inherits the shared echo/cursor/materialize/shuffle orchestration in
//! `qconnect_app::renderer` instead of re-deriving it.
//!
//! The protected bit-perfect seams (`play_streaming_dynamic` / `play_data`) and
//! the HTTP feeder live here, impl-side, exactly as the Tauri `CoreBridge` impl
//! does; the probe-derived sample_rate/channels/bit_depth flow STRAIGHT into
//! `play_streaming_dynamic` (never defaulted, or hi-res remote playback silently
//! resamples). The feeder body is a near-verbatim port of the Tauri
//! `track_loading.rs` feeder, with `bridge.player()` -> `self.core().player()`;
//! the only deviation is the TLS backend — the crates workspace `reqwest` ships
//! `rustls-tls` (not `native-tls`), so the `.use_native_tls()` calls are dropped.
//! TLS is transport encryption only; the decoded audio bytes are identical, so
//! bit-perfect is unaffected. (If the Qobuz streaming CDN ever presents a cert
//! rustls rejects, add `native-tls` to the frontend crate's reqwest features.)
//!
//! The feeder helpers (HEAD+range FLAC probe, chunked GET -> `BufferWriter`,
//! Akamai >100-header detection) are PRIVATE copies of the Slint crate's
//! `remote_stream.rs`, folded into this file per contract §1.3 — deliberately
//! NOT a shared module: the Slint crate keeps its own copy too, and extracting
//! commonalities into a shared crate is out of scope (backend untouchable).
//!
//! Wired by the QConnect facade and event sink. The module intentionally has
//! no blanket dead-code suppression: an orphaned renderer seam must warn.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use qbz_app::shell::AppRuntime;
use qbz_core::{CoreError, LoggingAdapter, QbzCore};
use qbz_models::{Quality, QueueTrack, RepeatMode, Track};
use qbz_player::{BufferWriter, PlaybackState, Player};
use qbz_qobuz::{ApiError, DelegatedQobuzClient};
use qconnect_app::{
    AuthorityActionPermit, AuthorityCell, AuthorityOrigin, AuthorityStamp, QconnectOwnerFailure,
    QconnectRendererEngine,
};

const RETIRED_AUTHORITY_ERROR: &str = "qconnect renderer authority is retired";

/// Credential boundary used for every catalog and stream-resolution request.
/// A delegated context never consults the owner's `QbzCore` Qobuz client.
pub enum RendererCatalogAuthority {
    Owner,
    Delegated(Arc<DelegatedQobuzClient>),
}

impl std::fmt::Debug for RendererCatalogAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Owner => formatter.write_str("Owner"),
            Self::Delegated(_) => formatter.write_str("Delegated(<redacted>)"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererAuthorityOrigin {
    Owner,
    Delegated,
}

/// Collapse owner API failures before they cross the QConnect engine seam.
/// Never format `error`: its nested reqwest URL or remote body can be sensitive.
fn classify_owner_api_failure(error: &ApiError) -> QconnectOwnerFailure {
    match error {
        ApiError::AuthenticationError(_)
        | ApiError::InvalidAppId
        | ApiError::InvalidAppSecret
        | ApiError::BundleExtractionError(_) => QconnectOwnerFailure::Authentication,
        ApiError::IneligibleUser | ApiError::Forbidden(_) | ApiError::ForbiddenCircuitOpen(_) => {
            QconnectOwnerFailure::Authorization
        }
        ApiError::NonStreamable
        | ApiError::InvalidQuality(_)
        | ApiError::NoQualityAvailable
        | ApiError::TrackUnavailable(_) => QconnectOwnerFailure::TrackUnavailable,
        ApiError::OfflineMode => QconnectOwnerFailure::Offline,
        ApiError::NetworkError(_) => QconnectOwnerFailure::Network,
        ApiError::ParseError(_) | ApiError::ApiResponse(_) => QconnectOwnerFailure::InvalidResponse,
        ApiError::RateLimited(_) => QconnectOwnerFailure::RateLimited,
        ApiError::ServerError(_) => QconnectOwnerFailure::Server,
    }
}

/// Typed owner-core classifier paired with [`classify_owner_api_failure`].
/// String-bearing variants are intentionally reduced to stable categories.
fn classify_owner_core_failure(error: &CoreError) -> QconnectOwnerFailure {
    match error {
        CoreError::AuthRequired | CoreError::AuthFailed(_) => QconnectOwnerFailure::Authentication,
        CoreError::Api(error) => classify_owner_api_failure(error),
        CoreError::Player(_) | CoreError::Playback(_) | CoreError::Audio(_) => {
            QconnectOwnerFailure::Playback
        }
        CoreError::Queue(_) | CoreError::Internal(_) => QconnectOwnerFailure::Internal,
        CoreError::NotInitialized => QconnectOwnerFailure::Unavailable,
    }
}

impl RendererCatalogAuthority {
    pub const fn origin(&self) -> RendererAuthorityOrigin {
        match self {
            Self::Owner => RendererAuthorityOrigin::Owner,
            Self::Delegated(_) => RendererAuthorityOrigin::Delegated,
        }
    }

    async fn get_track(
        &self,
        core: &QbzCore<LoggingAdapter>,
        track_id: u64,
    ) -> Result<Track, String> {
        match self {
            Self::Owner => core
                .get_track(track_id)
                .await
                .map_err(|err| classify_owner_core_failure(&err).to_string()),
            Self::Delegated(client) => client
                .get_track(track_id)
                .await
                .map_err(|err| format!("delegated catalog request for track {track_id}: {err}")),
        }
    }

    async fn get_tracks_batch(
        &self,
        core: &QbzCore<LoggingAdapter>,
        track_ids: &[u64],
    ) -> Result<Vec<Track>, String> {
        match self {
            Self::Owner => core
                .get_tracks_batch(track_ids)
                .await
                .map_err(|err| classify_owner_core_failure(&err).to_string()),
            Self::Delegated(client) => client
                .get_tracks_batch(track_ids)
                .await
                .map_err(|err| format!("delegated catalog batch request failed: {err}")),
        }
    }

    async fn get_stream_url(
        &self,
        core: &QbzCore<LoggingAdapter>,
        track_id: u64,
        quality: Quality,
    ) -> Result<qbz_models::StreamUrl, String> {
        match self {
            Self::Owner => core.get_stream_url(track_id, quality).await.map_err(|err| {
                format!(
                    "resolve owner stream for track {track_id}: {}",
                    classify_owner_core_failure(&err)
                )
            }),
            Self::Delegated(client) => client
                .get_stream_url_with_fallback(track_id, quality)
                .await
                .map_err(|err| format!("resolve delegated stream for track {track_id}: {err}")),
        }
    }
}

fn authority_origins_match(catalog: RendererAuthorityOrigin, stamp: AuthorityOrigin) -> bool {
    matches!(
        (catalog, stamp),
        (RendererAuthorityOrigin::Owner, AuthorityOrigin::Owner)
            | (
                RendererAuthorityOrigin::Delegated,
                AuthorityOrigin::Delegated { .. }
            )
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamRecoveryAction {
    FullDownload,
    OwnerCmaf,
    FailClosed,
}

fn recovery_action(origin: RendererAuthorityOrigin, header_flood: bool) -> StreamRecoveryAction {
    match (origin, header_flood) {
        (RendererAuthorityOrigin::Owner, true) => StreamRecoveryAction::OwnerCmaf,
        (RendererAuthorityOrigin::Delegated, true) => StreamRecoveryAction::FailClosed,
        (_, false) => StreamRecoveryAction::FullDownload,
    }
}

/// QConnect renderer engine backed by the Qt `AppRuntime`. Holds the shared
/// runtime and forwards every trait method through `runtime.core()`; the async
/// feeder spawns on the ambient tokio runtime (`start_track_stream` is always
/// awaited from a runtime task).
pub struct QtRendererEngine {
    runtime: Arc<AppRuntime<LoggingAdapter>>,
    catalog_authority: RendererCatalogAuthority,
    authority: Arc<AuthorityCell>,
    stamp: AuthorityStamp,
    /// The progressive HTTP task currently feeding the player. Replacing a
    /// track, stopping playback, or dropping this engine aborts the old task.
    active_feeder: std::sync::Mutex<Option<RemoteStreamFeeder>>,
}

impl QtRendererEngine {
    pub fn playback_event(&self) -> qbz_player::player::PlaybackEvent {
        self.runtime.core().player().get_playback_event()
    }

    pub fn owner(
        runtime: Arc<AppRuntime<LoggingAdapter>>,
        authority: Arc<AuthorityCell>,
        stamp: AuthorityStamp,
    ) -> Self {
        Self::with_authority(runtime, RendererCatalogAuthority::Owner, authority, stamp)
    }

    pub fn delegated(
        runtime: Arc<AppRuntime<LoggingAdapter>>,
        delegated_client: Arc<DelegatedQobuzClient>,
        authority: Arc<AuthorityCell>,
        stamp: AuthorityStamp,
    ) -> Self {
        Self::with_authority(
            runtime,
            RendererCatalogAuthority::Delegated(delegated_client),
            authority,
            stamp,
        )
    }

    fn with_authority(
        runtime: Arc<AppRuntime<LoggingAdapter>>,
        catalog_authority: RendererCatalogAuthority,
        authority: Arc<AuthorityCell>,
        stamp: AuthorityStamp,
    ) -> Self {
        assert!(
            authority_origins_match(catalog_authority.origin(), stamp.origin()),
            "renderer catalog authority must match its runtime authority stamp"
        );
        Self {
            runtime,
            catalog_authority,
            authority,
            stamp,
            active_feeder: std::sync::Mutex::new(None),
        }
    }

    pub const fn authority_origin(&self) -> RendererAuthorityOrigin {
        self.catalog_authority.origin()
    }

    fn is_current(&self) -> bool {
        self.authority.is_current(self.stamp)
            && (!matches!(self.stamp.origin(), AuthorityOrigin::Owner)
                || self.authority.owner_actions_allowed())
    }

    fn ensure_current(&self) -> Result<(), String> {
        if self.is_current() {
            Ok(())
        } else {
            Err(RETIRED_AUTHORITY_ERROR.to_string())
        }
    }

    fn action_permit(&self) -> Result<AuthorityActionPermit, String> {
        self.authority
            .try_runtime_action_permit(self.stamp)
            .ok_or_else(|| RETIRED_AUTHORITY_ERROR.to_string())
    }

    fn core(&self) -> &Arc<QbzCore<LoggingAdapter>> {
        self.runtime.core()
    }

    fn replace_active_feeder(&self, feeder: RemoteStreamFeeder) {
        let previous = self
            .active_feeder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(feeder);
        drop(previous);
    }

    fn cancel_active_feeder(&self) {
        let feeder = self
            .active_feeder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(feeder);
    }

    /// Last-resort load for tracks the raw-URL path cannot fetch (the CDN
    /// header flood defeats every reqwest attempt — see
    /// `is_header_flood_error`): the CMAF path is unaffected by the h1 header
    /// cap. `play_track_resolved` does NOT move the queue cursor (nothing on
    /// the QConnect path does — the shared driver's cursor sync only fires on a
    /// playing->playing track edge), so sync it explicitly or the now-playing
    /// truth keeps showing the PREVIOUS track while the recovered one plays.
    async fn play_via_owner_cmaf(
        &self,
        track_id: u64,
        quality: Quality,
        start_position_secs: u64,
        playing: bool,
    ) -> Result<(), String> {
        let _permit = self.action_permit()?;
        if self.authority_origin() != RendererAuthorityOrigin::Owner {
            return Err(format!(
                "delegated stream for track {track_id} cannot use owner CMAF fallback"
            ));
        }
        let playback_result = self
            .core()
            .play_track_resolved_with_state(track_id, quality, None, None, start_position_secs, playing)
            .await;
        self.ensure_current()?;
        playback_result.map_err(|error| {
            format!(
                "CMAF fallback for remote track {track_id}: {}",
                QconnectOwnerFailure::from_opaque_playback_error(&error)
            )
        })?;
        self.core().sync_current_to_id(track_id).await;
        self.ensure_current()?;
        Ok(())
    }
}

impl Drop for QtRendererEngine {
    fn drop(&mut self) {
        self.cancel_active_feeder();
    }
}

#[async_trait]
impl QconnectRendererEngine for QtRendererEngine {
    // ---- transport (sync) ----
    fn resume(&self) -> Result<(), String> {
        let _permit = self.action_permit()?;
        self.core().resume().map_err(|err| err.to_string())
    }
    fn pause(&self) -> Result<(), String> {
        let _permit = self.action_permit()?;
        self.core().pause().map_err(|err| err.to_string())
    }
    fn stop(&self) -> Result<(), String> {
        self.cancel_active_feeder();
        let _permit = self.action_permit()?;
        self.core().stop().map_err(|err| err.to_string())
    }
    fn seek(&self, position_secs: u64) -> Result<(), String> {
        let _permit = self.action_permit()?;
        self.core()
            .seek(position_secs)
            .map_err(|err| err.to_string())
    }
    fn set_volume(&self, fraction: f32) -> Result<(), String> {
        let _permit = self.action_permit()?;
        self.core()
            .set_volume(fraction)
            .map_err(|err| err.to_string())
    }
    fn get_playback_state(&self) -> PlaybackState {
        if self.is_current() {
            self.core().get_playback_state()
        } else {
            PlaybackState::default()
        }
    }
    fn loading_state(&self) -> Option<(u64, qbz_player::player::PlaybackBufferState)> {
        let event = self.playback_event();
        Some((event.buffer_track_id, event.buffer_state))
    }

    fn has_loaded_audio(&self) -> bool {
        self.is_current() && self.core().player().has_loaded_audio()
    }

    // ---- queue / mode (async) ----
    async fn set_repeat_mode(&self, mode: RepeatMode) {
        if let Ok(_permit) = self.action_permit() {
            self.core().set_repeat_mode(mode).await;
        }
    }
    async fn set_shuffle(&self, enabled: bool) {
        if let Ok(_permit) = self.action_permit() {
            self.core().set_shuffle(enabled).await;
        }
    }
    async fn get_all_queue_tracks(&self) -> (Vec<QueueTrack>, Option<usize>) {
        if !self.is_current() {
            return (Vec::new(), None);
        }
        let tracks = self.core().get_all_queue_tracks().await;
        if self.is_current() {
            tracks
        } else {
            (Vec::new(), None)
        }
    }
    async fn set_queue(&self, tracks: Vec<QueueTrack>, start_index: Option<usize>) {
        if let Ok(_permit) = self.action_permit() {
            self.core().set_queue(tracks, start_index).await;
        }
    }
    async fn set_queue_with_order(
        &self,
        tracks: Vec<QueueTrack>,
        start_index: Option<usize>,
        shuffle_enabled: bool,
        shuffle_order: Option<Vec<usize>>,
    ) {
        if let Ok(_permit) = self.action_permit() {
            self.core()
                .set_queue_with_order(tracks, start_index, shuffle_enabled, shuffle_order)
                .await;
        }
    }
    async fn clear_queue(&self, keep_current: bool) {
        if let Ok(_permit) = self.action_permit() {
            self.core().clear_queue(keep_current).await;
        }
    }
    async fn play_index(&self, index: usize) -> Option<QueueTrack> {
        let _permit = self.action_permit().ok()?;
        self.core().play_index(index).await
    }

    // ---- catalog (async) ----
    async fn get_track(&self, track_id: u64) -> Result<Track, String> {
        self.ensure_current()?;
        let result = self
            .catalog_authority
            .get_track(self.core(), track_id)
            .await;
        self.ensure_current()?;
        result
    }
    async fn get_tracks_batch(&self, track_ids: &[u64]) -> Result<Vec<Track>, String> {
        self.ensure_current()?;
        let result = self
            .catalog_authority
            .get_tracks_batch(self.core(), track_ids)
            .await;
        self.ensure_current()?;
        result
    }

    // ---- protected audio seam (the only protected touch) ----
    async fn start_track_stream(
        &self,
        track_id: u64,
        quality: Quality,
        duration_secs: u64,
        start_position_secs: u64,
    ) -> Result<(), String> {
        self.start_track_stream_with_state(track_id, quality, duration_secs, start_position_secs, true).await
    }

    async fn start_track_stream_with_state(
        &self,
        track_id: u64,
        quality: Quality,
        duration_secs: u64,
        start_position_secs: u64,
        playing: bool,
    ) -> Result<(), String> {
        self.cancel_active_feeder();
        let _permit = self.action_permit()?;
        let stream_url_result = self
            .catalog_authority
            .get_stream_url(self.core(), track_id, quality)
            .await;
        self.ensure_current()?;
        let stream_url = stream_url_result?;

        let player = self.core().player();
        let stream_result = stream_remote_track_into_player(
            &player,
            track_id,
            duration_secs,
            start_position_secs,
            playing,
            &stream_url.url,
            "QConnect",
            || self.ensure_current(),
        )
        .await;
        self.ensure_current()?;

        let stream_err = match stream_result {
            Ok(feeder) => {
                self.ensure_current()?;
                self.replace_active_feeder(feeder);
                if let Err(error) = self.ensure_current() {
                    self.cancel_active_feeder();
                    return Err(error);
                }
                return Ok(());
            }
            Err(error) => error,
        };

        let header_flood = is_header_flood_error(&stream_err);
        match recovery_action(self.authority_origin(), header_flood) {
            StreamRecoveryAction::OwnerCmaf => {
                log::warn!(
                    "[QConnect] Owner raw-URL streaming hit the CDN header limit for track {track_id}: {stream_err}. Skipping full download; last resort: CMAF."
                );
                return self
                    .play_via_owner_cmaf(track_id, quality, start_position_secs, playing)
                    .await;
            }
            StreamRecoveryAction::FailClosed => {
                log::warn!(
                    "[QConnect] Delegated raw-URL streaming hit the CDN header limit for track {track_id}: {stream_err}. Owner and CMAF fallback are forbidden."
                );
                return Err(format!(
                    "delegated CDN stream for track {track_id} failed: {stream_err}"
                ));
            }
            StreamRecoveryAction::FullDownload => {}
        }

        log::warn!(
            "[QConnect] Streaming handoff unavailable for track {}: {}. Falling back to full download.",
            track_id,
            stream_err
        );
        match download_remote_audio(&stream_url.url).await {
            Ok(audio_data) => {
                self.ensure_current()?;
                let playback_result = self.core().player().play_data_at(audio_data, track_id, start_position_secs, playing);
                self.ensure_current()?;
                playback_result.map_err(|err| format!("play remote track {track_id}: {err}"))?;
                Ok(())
            }
            Err(download_err)
                if recovery_action(
                    self.authority_origin(),
                    is_header_flood_error(&download_err),
                ) == StreamRecoveryAction::OwnerCmaf =>
            {
                log::warn!(
                    "[QConnect] Full download hit the CDN header flood for track {track_id}: {download_err}. Last resort: CMAF."
                );
                self.play_via_owner_cmaf(track_id, quality, start_position_secs, playing)
                    .await
            }
            Err(download_err) => Err(download_err),
        }
    }

    fn current_output_format(&self) -> Option<(u32, u32)> {
        if !self.is_current() {
            return None;
        }
        let player = self.core().player();
        Some((player.state.get_sample_rate(), player.state.get_bit_depth()))
    }
}

async fn download_remote_audio(url: &str) -> Result<Vec<u8>, String> {
    let response = reqwest::Client::new()
        .get(url)
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await
        .map_err(|err| {
            format!(
                "download remote audio request failed: {}",
                describe_reqwest_error(&err)
            )
        })?;

    if !response.status().is_success() {
        return Err(format!(
            "download remote audio failed with status {}",
            response.status()
        ));
    }

    let bytes = response.bytes().await.map_err(|err| {
        format!(
            "read remote audio bytes failed: {}",
            describe_reqwest_error(&err)
        )
    })?;
    Ok(bytes.to_vec())
}

// ---------------------------------------------------------------------------
// HTTP feeder helpers. Progressive feeder: probe a remote audio URL for size
// + FLAC format, open the player's progressive streaming sink
// (`Player::play_streaming_dynamic`), then push the body to the returned
// `BufferWriter` chunk-by-chunk as it arrives. Playback starts as soon as the
// initial buffer fills — not after the whole file lands.
//
// BIT-PERFECT: `play_streaming_dynamic` decodes the same original bytes and
// drives the PROTECTED device init from the decoded stream. The
// `sample_rate`/`bit_depth` parsed here are only the streaming-config hints;
// the audio backend (`pipewire_backend.rs`, `init_device`, `audio_settings.rs`)
// is untouched.
// ---------------------------------------------------------------------------

/// Owned background feeder for one progressive remote stream.
///
/// Tokio detaches a task when its bare `JoinHandle` is dropped. This wrapper
/// deliberately aborts instead: the renderer owns exactly one feeder, so an
/// old CDN request cannot keep writing after replacement or authority retire.
#[must_use = "dropping the feeder aborts the background CDN request"]
struct RemoteStreamFeeder {
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
struct RemoteStreamInfo {
    content_length: u64,
    sample_rate: u32,
    channels: u16,
    bit_depth: u32,
    speed_mbps: f64,
}

/// Probe + open the progressive sink + spawn the background feeder.
///
/// On success the player has begun buffering and `play_streaming_dynamic` will
/// start audio once the initial buffer fills; the body download runs in a
/// spawned task. Errors here mean the caller should fall back to a full
/// download (the probe or the sink open failed).
async fn stream_remote_track_into_player(
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
async fn probe_remote_stream_info(url: &str) -> Result<RemoteStreamInfo, String> {
    use std::time::Instant;

    let client = reqwest::Client::builder()
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

    // STREAMINFO parse via the shared prober (hoisted to qbz-models for the
    // cast path, #638 fix 1). The prober never guesses — the CD-shaped
    // defaults for a non-FLAC probe stay HERE so this path's behavior is
    // byte-identical to the original inline parse.
    let (sample_rate, channels, bit_depth) = match qbz_models::probe_streaminfo(&initial_bytes) {
        Some(p) => (p.sample_rate, p.channels, p.bits_per_sample),
        None => {
            log::warn!("[remote-stream] Non-FLAC probe for remote handoff, using defaults");
            (44_100, 2, 16)
        }
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
async fn download_and_stream_remote_track(
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

    let client = reqwest::Client::builder()
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
/// its query). Inspect the chain only to retain header-limit classification;
/// never copy an arbitrary cause into logs or a returned error.
fn describe_reqwest_error(err: &reqwest::Error) -> String {
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
fn is_header_flood_error(message: &str) -> bool {
    let haystack = message.to_ascii_lowercase();
    haystack.contains("message head is too large") || haystack.contains("too many headers")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGNED_URL_MARKER: &str =
        "https://api.example.test/track/get?request_ts=42&request_sig=SIGNED-URL-SECRET";
    const REMOTE_BODY_MARKER: &str = "REMOTE-BODY\nAuthorization: Bearer jwt_api-secret";

    #[test]
    fn owner_api_failure_categories_discard_signed_urls_and_remote_bodies() {
        let errors = [
            ApiError::AuthenticationError(SIGNED_URL_MARKER.to_string()),
            ApiError::Forbidden(format!(" : {REMOTE_BODY_MARKER}")),
            ApiError::ApiResponse(format!("{REMOTE_BODY_MARKER} {SIGNED_URL_MARKER}")),
        ];

        for error in errors {
            let diagnostic = classify_owner_api_failure(&error).to_string();
            assert!(!diagnostic.contains("SIGNED-URL-SECRET"));
            assert!(!diagnostic.contains("REMOTE-BODY"));
            assert!(!diagnostic.contains("Bearer"));
            assert!(!diagnostic.contains("jwt_api-secret"));
            assert!(!diagnostic.contains('\n'));
        }
    }

    #[test]
    fn owner_core_and_opaque_playback_failures_are_payload_free() {
        let core_error = CoreError::Api(ApiError::ApiResponse(format!(
            "{SIGNED_URL_MARKER} {REMOTE_BODY_MARKER}"
        )));
        let core_diagnostic = classify_owner_core_failure(&core_error).to_string();
        assert_eq!(
            core_diagnostic,
            QconnectOwnerFailure::InvalidResponse.to_string()
        );
        assert!(!core_diagnostic.contains("SIGNED-URL-SECRET"));
        assert!(!core_diagnostic.contains("REMOTE-BODY"));

        let opaque_error = format!("{SIGNED_URL_MARKER} {REMOTE_BODY_MARKER}");
        let playback_diagnostic =
            QconnectOwnerFailure::from_opaque_playback_error(&opaque_error).to_string();
        assert_eq!(
            playback_diagnostic,
            QconnectOwnerFailure::Playback.to_string()
        );
        assert!(!playback_diagnostic.contains("SIGNED-URL-SECRET"));
        assert!(!playback_diagnostic.contains("REMOTE-BODY"));
    }

    #[test]
    fn delegated_header_flood_fails_closed_without_owner_cmaf() {
        assert_eq!(
            recovery_action(RendererAuthorityOrigin::Delegated, true),
            StreamRecoveryAction::FailClosed
        );
        assert_ne!(
            recovery_action(RendererAuthorityOrigin::Delegated, true),
            StreamRecoveryAction::OwnerCmaf
        );
    }

    #[test]
    fn delegated_non_header_failure_only_reuses_the_delegated_cdn_url() {
        assert_eq!(
            recovery_action(RendererAuthorityOrigin::Delegated, false),
            StreamRecoveryAction::FullDownload
        );
    }

    #[test]
    fn owner_retains_cmaf_recovery_for_the_known_header_limit() {
        assert_eq!(
            recovery_action(RendererAuthorityOrigin::Owner, true),
            StreamRecoveryAction::OwnerCmaf
        );
    }

    #[test]
    fn catalog_origin_must_match_the_runtime_stamp_origin() {
        assert!(authority_origins_match(
            RendererAuthorityOrigin::Owner,
            AuthorityOrigin::Owner
        ));
        assert!(authority_origins_match(
            RendererAuthorityOrigin::Delegated,
            AuthorityOrigin::Delegated { generation: 9 }
        ));
        assert!(!authority_origins_match(
            RendererAuthorityOrigin::Owner,
            AuthorityOrigin::Delegated { generation: 9 }
        ));
        assert!(!authority_origins_match(
            RendererAuthorityOrigin::Delegated,
            AuthorityOrigin::Owner
        ));
    }

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
