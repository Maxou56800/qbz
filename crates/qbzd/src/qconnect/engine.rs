// TODO(converge: qconnect-glue) — copied from crates/qbz/src/qconnect_engine.rs @ c8ef2a1b;
// do not fix bugs here without fixing the source, and vice versa.
//
//! Qobuz Connect renderer engine for the qbzd daemon.
//!
//! Implements [`qconnect_app::QconnectRendererEngine`] over the daemon
//! `AppRuntime`'s `QbzCore` + `Player`, so qbzd becomes a QConnect renderer
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
//! rustls rejects, add `native-tls` to qbzd's reqwest features.)
#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use qbz_app::shell::AppRuntime;
use qbz_core::{CoreError, QbzCore};
use qbz_models::{Quality, QueueTrack, RepeatMode, Track};
use qbz_player::PlaybackState;
use qbz_qobuz::{ApiError, DelegatedQobuzClient};
use qconnect_app::{QconnectOwnerFailure, QconnectRendererEngine};

use crate::adapter::DaemonAdapter;

use super::authority::{AuthorityActionPermit, AuthorityCell, AuthorityOrigin, AuthorityStamp};

const RETIRED_AUTHORITY_ERROR: &str = "qconnect renderer authority is retired";

// T10 (OD4, §7.4): daemon volume policy. The mode is read from the daemon-root
// `qconnect_settings.db` `volume_mode` KV key (transport::load_volume_mode_at)
// at connect time and injected into the engine + session host.
/// How the daemon treats a controller's remote volume command (01 §7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VolumeMode {
    /// OD4 DEFAULT. Remote `SetVolume` is applied to the player via the core,
    /// and the player's real volume is reported back to the controller.
    #[default]
    Software,
    /// Bit-perfect purist. The player stays at 100 % (no software attenuation);
    /// remote volume/mute commands are ignored before state changes or reports.
    /// The session reports 100. For DACs where software gain is unwanted.
    Locked,
}

impl VolumeMode {
    /// Parse the `volume_mode` KV value. Anything but the literal `"locked"`
    /// (unset, empty, unknown) falls back to `Software` — the OD4 default.
    pub fn from_kv(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("locked") => VolumeMode::Locked,
            _ => VolumeMode::Software,
        }
    }

    /// Whether a controller's remote `SetVolume` should reach the player. True
    /// only in `Software`; `Locked` acknowledges-but-ignores.
    pub fn applies_remote_volume(self) -> bool {
        matches!(self, VolumeMode::Software)
    }

    /// The volume (0-100 percent) to REPORT to the controller given the player's
    /// real 0.0-1.0 fraction. `Software` reports the real (rounded) percent;
    /// `Locked` always reports 100 regardless of the player's actual level.
    pub fn reported_volume_pct(self, real_fraction: f32) -> i32 {
        match self {
            VolumeMode::Software => (real_fraction.clamp(0.0, 1.0) * 100.0).round() as i32,
            VolumeMode::Locked => 100,
        }
    }
}

/// Credential boundary used for all Qobuz catalog and stream-resolution calls
/// made by this renderer engine.
///
/// A delegated client is a complete, isolated API context. Keeping the origin
/// in this enum makes it impossible for the guest path to silently consult the
/// owner's `QbzCore` API client when delegated metadata or stream resolution
/// fails.
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
        ApiError::NetworkError(_) | ApiError::InvalidProxyConfig(_) => {
            QconnectOwnerFailure::Network
        }
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
        core: &QbzCore<DaemonAdapter>,
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
        core: &QbzCore<DaemonAdapter>,
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
        core: &QbzCore<DaemonAdapter>,
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

/// QConnect renderer engine backed by the daemon `AppRuntime`. Holds the shared
/// runtime and forwards every trait method through `runtime.core()`; the async
/// feeder spawns on the ambient tokio runtime (`start_track_stream` is always
/// awaited from a runtime task).
pub struct DaemonRendererEngine {
    runtime: Arc<AppRuntime<DaemonAdapter>>,
    catalog_authority: RendererCatalogAuthority,
    authority: Arc<AuthorityCell>,
    stamp: AuthorityStamp,
    /// T10 (OD4): resolved volume policy for this session (from the KV at connect).
    volume_mode: VolumeMode,
    /// Live daemon `playback.quality` preference. The settings reload route
    /// updates this same cell, so an active QConnect session honors changes
    /// without reconnecting (#693).
    quality_cap: Arc<std::sync::Mutex<Quality>>,
    /// The progressive HTTP task currently feeding the player. Replacing a
    /// track, stopping playback, or dropping this engine aborts the old task so
    /// a retired authority cannot keep writing bytes in the background.
    active_feeder: std::sync::Mutex<Option<super::remote_stream::RemoteStreamFeeder>>,
}

impl DaemonRendererEngine {
    pub fn volume_mode(&self) -> VolumeMode {
        self.volume_mode
    }

    pub fn playback_event(&self) -> qbz_player::player::PlaybackEvent {
        self.runtime.core().player().get_playback_event()
    }

    /// Compatibility alias for the pre-delegation owner constructor. New
    /// integration code should call [`Self::owner`] or [`Self::delegated`] so
    /// the credential origin is explicit at the construction boundary.
    pub fn new(
        runtime: Arc<AppRuntime<DaemonAdapter>>,
        volume_mode: VolumeMode,
        quality_cap: Arc<std::sync::Mutex<Quality>>,
        authority: Arc<AuthorityCell>,
        stamp: AuthorityStamp,
    ) -> Self {
        Self::owner(runtime, volume_mode, quality_cap, authority, stamp)
    }

    pub fn owner(
        runtime: Arc<AppRuntime<DaemonAdapter>>,
        volume_mode: VolumeMode,
        quality_cap: Arc<std::sync::Mutex<Quality>>,
        authority: Arc<AuthorityCell>,
        stamp: AuthorityStamp,
    ) -> Self {
        Self::with_authority(
            runtime,
            RendererCatalogAuthority::Owner,
            volume_mode,
            quality_cap,
            authority,
            stamp,
        )
    }

    pub fn delegated(
        runtime: Arc<AppRuntime<DaemonAdapter>>,
        delegated_client: Arc<DelegatedQobuzClient>,
        volume_mode: VolumeMode,
        quality_cap: Arc<std::sync::Mutex<Quality>>,
        authority: Arc<AuthorityCell>,
        stamp: AuthorityStamp,
    ) -> Self {
        Self::with_authority(
            runtime,
            RendererCatalogAuthority::Delegated(delegated_client),
            volume_mode,
            quality_cap,
            authority,
            stamp,
        )
    }

    fn with_authority(
        runtime: Arc<AppRuntime<DaemonAdapter>>,
        catalog_authority: RendererCatalogAuthority,
        volume_mode: VolumeMode,
        quality_cap: Arc<std::sync::Mutex<Quality>>,
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
            volume_mode,
            quality_cap,
            active_feeder: std::sync::Mutex::new(None),
        }
    }

    pub const fn authority_origin(&self) -> RendererAuthorityOrigin {
        self.catalog_authority.origin()
    }

    pub const fn authority_stamp(&self) -> AuthorityStamp {
        self.stamp
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

    fn core(&self) -> &Arc<QbzCore<DaemonAdapter>> {
        self.runtime.core()
    }

    fn effective_quality(&self, requested: Quality) -> Quality {
        let cap = self
            .quality_cap
            .lock()
            .map(|quality| *quality)
            .unwrap_or_else(|poisoned| *poisoned.into_inner());
        clamp_quality(requested, cap)
    }

    fn replace_active_feeder(&self, feeder: super::remote_stream::RemoteStreamFeeder) {
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
    /// `remote_stream::is_header_flood_error`): the CMAF path is unaffected by
    /// the h1 header cap. `play_track_resolved` does NOT move the queue cursor
    /// (nothing on the QConnect path does — the shared driver's cursor sync
    /// only fires on a playing->playing track edge), so sync it explicitly or
    /// `qbzd status` / the local now-playing truth keep showing the PREVIOUS
    /// track while the recovered one plays.
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

impl Drop for DaemonRendererEngine {
    fn drop(&mut self) {
        self.cancel_active_feeder();
    }
}

#[async_trait]
impl QconnectRendererEngine for DaemonRendererEngine {
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
        // T10 (OD4, §7.4): volume-mode gate. In `Locked` mode the player stays
        // at 100 % and a controller's remote SetVolume is acknowledged-but-
        // ignored (logged at info), so the DAC keeps receiving full-scale,
        // bit-perfect samples. `Software` (default) applies it via the core.
        if !self.volume_mode.applies_remote_volume() {
            log::info!(
                "[QConnect] volume_mode=locked: ignoring remote SetVolume({:.3}); player stays at 100%",
                fraction
            );
            return Ok(());
        }
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
        let track = self.core().play_index(index).await;
        track
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
        requested_quality: Quality,
        duration_secs: u64,
        start_position_secs: u64,
    ) -> Result<(), String> {
        self.start_track_stream_with_state(track_id, requested_quality, duration_secs, start_position_secs, true).await
    }

    async fn start_track_stream_with_state(
        &self,
        track_id: u64,
        requested_quality: Quality,
        duration_secs: u64,
        start_position_secs: u64,
        playing: bool,
    ) -> Result<(), String> {
        self.cancel_active_feeder();
        let _permit = self.action_permit()?;
        let quality = self.effective_quality(requested_quality);
        if quality != requested_quality {
            log::info!(
                "[QConnect] playback.quality capped track {track_id}: controller requested {:?}, using {:?}",
                requested_quality,
                quality
            );
        }
        let stream_url_result = self
            .catalog_authority
            .get_stream_url(self.core(), track_id, quality)
            .await;
        self.ensure_current()?;
        let stream_url = stream_url_result?;

        let player = self.core().player();
        let stream_result = super::remote_stream::stream_remote_track_into_player(
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

        let header_flood = super::remote_stream::is_header_flood_error(&stream_err);
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
                    super::remote_stream::is_header_flood_error(&download_err),
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

fn clamp_quality(requested: Quality, cap: Quality) -> Quality {
    Quality::min_tier(requested, cap)
}

async fn download_remote_audio(url: &str) -> Result<Vec<u8>, String> {
    let client = qbz_net_proxy::apply_current(reqwest::Client::builder())
        .and_then(|builder| builder.build().map_err(Into::into))
        .unwrap_or_default();
    let response = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await
        .map_err(|err| {
            format!(
                "download remote audio request failed: {}",
                super::remote_stream::describe_reqwest_error(&err)
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
            super::remote_stream::describe_reqwest_error(&err)
        )
    })?;
    Ok(bytes.to_vec())
}

// T10 (OD4, §7.4): volume-mode policy tests. These pin the decision the engine's
// `set_volume` gate and the session host's join-time volume report consult — the
// two enforcement points of the software|locked contract.
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
    fn software_mode_applies_and_reports_real() {
        // remote SetVolume 0.4 -> engine.set_volume(0.4); report reads real volume.
        let mode = VolumeMode::from_kv(Some("software"));
        assert_eq!(mode, VolumeMode::Software);
        assert!(mode.applies_remote_volume());
        assert_eq!(mode.reported_volume_pct(0.4), 40);
        assert_eq!(mode.reported_volume_pct(1.0), 100);
    }

    #[test]
    fn locked_mode_ignores_and_reports_100() {
        // remote SetVolume -> acknowledged-but-ignored; player stays 1.0; 100 reported.
        let mode = VolumeMode::from_kv(Some("locked"));
        assert_eq!(mode, VolumeMode::Locked);
        assert!(!mode.applies_remote_volume());
        // 100 reported regardless of the player's actual level.
        assert_eq!(mode.reported_volume_pct(0.4), 100);
        assert_eq!(mode.reported_volume_pct(1.0), 100);
    }

    #[test]
    fn default_mode_is_software_od4() {
        // Unset / empty / unknown all resolve to the OD4 default (software).
        assert_eq!(VolumeMode::default(), VolumeMode::Software);
        assert_eq!(VolumeMode::from_kv(None), VolumeMode::Software);
        assert_eq!(VolumeMode::from_kv(Some("")), VolumeMode::Software);
        assert_eq!(VolumeMode::from_kv(Some("  ")), VolumeMode::Software);
        assert_eq!(VolumeMode::from_kv(Some("garbage")), VolumeMode::Software);
        // Whitespace around the real value is tolerated.
        assert_eq!(VolumeMode::from_kv(Some(" locked ")), VolumeMode::Locked);
    }

    #[test]
    fn qconnect_quality_never_exceeds_daemon_preference() {
        let tiers = [
            Quality::Mp3,
            Quality::Lossless,
            Quality::HiRes,
            Quality::UltraHiRes,
        ];
        for requested in tiers {
            for cap in tiers {
                let effective = clamp_quality(requested, cap);
                assert!(effective <= requested);
                assert!(effective <= cap);
                assert_eq!(effective, requested.min(cap));
            }
        }
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
}
