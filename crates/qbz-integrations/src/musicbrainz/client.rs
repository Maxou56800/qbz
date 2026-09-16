//! MusicBrainz API client
//!
//! HTTP client with rate limiting and proper User-Agent handling.
//! Uses Cloudflare Workers proxy for consistent rate limiting.

use reqwest::Client;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use super::identity;
use super::models::*;
use crate::error::{IntegrationError, IntegrationResult};

/// Proxy URL for MusicBrainz requests
const MUSICBRAINZ_PROXY_URL: &str = "https://qbz-api-proxy.blitzkriegfc.workers.dev/musicbrainz";

/// Direct MusicBrainz API URL (fallback)
const MUSICBRAINZ_API_URL: &str = "https://musicbrainz.org/ws/2";

/// The ONE direct-MusicBrainz limiter for this whole process (piece 2). Every
/// `MusicBrainzClient` on the default direct path shares it, so several clients
/// on one public IP (core, the tag editor, remote metadata) pace as a single
/// 1.1s stream instead of each pacing itself and together exceeding MB's per-IP
/// budget -- the funnel behind the 503s. The proxy path is unaffected.
static SHARED_DIRECT: OnceLock<Arc<RateLimiter>> = OnceLock::new();

/// Rate limiter for MusicBrainz API
pub struct RateLimiter {
    last_request: Mutex<Instant>,
    min_interval: Duration,
}

impl RateLimiter {
    /// Create rate limiter for direct MusicBrainz API (1 req/sec)
    pub fn new() -> Self {
        Self::with_interval(Duration::from_millis(1100))
    }

    /// The process-wide SHARED direct limiter (1.1s). See [`SHARED_DIRECT`].
    /// Every direct client shares this one so they pace as a single stream.
    pub fn shared() -> Arc<RateLimiter> {
        SHARED_DIRECT
            .get_or_init(|| Arc::new(RateLimiter::new()))
            .clone()
    }

    /// Create rate limiter for proxy (faster, proxy handles actual rate limiting)
    pub fn for_proxy() -> Self {
        Self::with_interval(Duration::from_millis(200))
    }

    /// Create rate limiter with custom interval
    pub fn with_interval(min_interval: Duration) -> Self {
        Self {
            // Start in the past so first request doesn't wait
            last_request: Mutex::new(Instant::now() - Duration::from_secs(2)),
            min_interval,
        }
    }

    pub async fn wait(&self) {
        let mut last = self.last_request.lock().await;
        let elapsed = last.elapsed();
        if elapsed < self.min_interval {
            tokio::time::sleep(self.min_interval - elapsed).await;
        }
        *last = Instant::now();
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// MusicBrainz API client configuration
#[derive(Debug, Clone)]
pub struct MusicBrainzConfig {
    /// Whether MusicBrainz integration is enabled
    pub enabled: bool,
    /// Use proxy instead of direct API
    pub use_proxy: bool,
}

impl Default for MusicBrainzConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            // Direct-to-MusicBrainz by default: each client uses its OWN IP, so
            // the per-IP 1 req/s budget is per-user instead of shared across all
            // QBZ users behind the proxy's Cloudflare egress IPs (which is what
            // triggered the 503s). MB read access needs no key, so the proxy
            // added only the funnel. Flip to true to route via the proxy again.
            use_proxy: false,
        }
    }
}

/// MusicBrainz API client
pub struct MusicBrainzClient {
    client: Client,
    rate_limiter: Arc<RateLimiter>,
    config: Arc<Mutex<MusicBrainzConfig>>,
}

impl Default for MusicBrainzClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MusicBrainzClient {
    /// Create a new MusicBrainz client with default config
    pub fn new() -> Self {
        Self::with_config(MusicBrainzConfig::default())
    }

    /// Create client with specific configuration
    pub fn with_config(config: MusicBrainzConfig) -> Self {
        let version = "1.0.0";
        let user_agent = format!(
            "QBZ/{} (https://github.com/vicrodh/qbz; qbz@vicrodh.dev)",
            version
        );

        let builder =
            qbz_net_proxy::apply_current(Client::builder()).unwrap_or_else(|_| Client::builder());
        let client = builder
            .user_agent(&user_agent)
            .timeout(Duration::from_secs(6))
            .build()
            .unwrap_or_else(|_| Client::new());

        // The direct-MusicBrainz limiter is process-wide and SHARED across
        // every client (piece 2); the proxy path keeps its own faster one.
        let rate_limiter = if config.use_proxy {
            Arc::new(RateLimiter::for_proxy())
        } else {
            RateLimiter::shared()
        };

        Self {
            client,
            rate_limiter,
            config: Arc::new(Mutex::new(config)),
        }
    }

    /// Check if MusicBrainz integration is enabled
    pub async fn is_enabled(&self) -> bool {
        self.config.lock().await.enabled
    }

    /// Enable or disable MusicBrainz integration
    pub async fn set_enabled(&self, enabled: bool) {
        self.config.lock().await.enabled = enabled;
    }

    /// Get the base URL based on configuration
    async fn base_url(&self) -> &'static str {
        if self.config.lock().await.use_proxy {
            MUSICBRAINZ_PROXY_URL
        } else {
            MUSICBRAINZ_API_URL
        }
    }

    /// Search recordings by ISRC
    pub async fn search_recording_by_isrc(
        &self,
        isrc: &str,
    ) -> IntegrationResult<RecordingSearchResponse> {
        if !self.is_enabled().await {
            return Err(IntegrationError::ServiceUnavailable(
                "MusicBrainz integration is disabled".into(),
            ));
        }


        let base = self.base_url().await;
        let url = format!("{}/recording?query=isrc:{}&fmt=json", base, isrc);

        let response = self.send_with_retry(&url).await?;
        response.json().await.map_err(Into::into)
    }

    /// Search artists by name
    pub async fn search_artist(
        &self,
        name: &str,
        limit: u32,
    ) -> IntegrationResult<ArtistSearchResponse> {
        if !self.is_enabled().await {
            return Err(IntegrationError::ServiceUnavailable(
                "MusicBrainz integration is disabled".into(),
            ));
        }


        // Exact quoted field expression, URL-encoded exactly once (#768:
        // `artist:Swim Deep` let "Deep" leak into the global query and ranked
        // Deep Purple first at score 100).
        let base = self.base_url().await;
        let url = identity::artist_search_url(base, name, limit);

        let response = self.send_with_retry(&url).await?;
        response.json().await.map_err(Into::into)
    }

    /// Resolve a track to get MusicBrainz IDs
    ///
    /// Searches by ISRC if available, falling back to text search.
    pub async fn resolve_track(
        &self,
        artist: &str,
        title: &str,
        isrc: Option<&str>,
    ) -> IntegrationResult<Option<ResolvedTrack>> {
        // Try ISRC first (most accurate)
        if let Some(isrc) = isrc {
            let response = self.search_recording_by_isrc(isrc).await?;
            if let Some(recording) = response.recordings.first() {
                let confidence = if recording
                    .isrcs
                    .as_ref()
                    .map_or(false, |isrcs| isrcs.contains(&isrc.to_string()))
                {
                    MatchConfidence::Exact
                } else {
                    MatchConfidence::from_score(recording.score)
                };

                return Ok(Some(ResolvedTrack {
                    recording_mbid: recording.id.clone(),
                    title: recording.title.clone().unwrap_or_default(),
                    artist_mbids: recording
                        .artist_credit
                        .as_ref()
                        .map(|ac| ac.iter().map(|a| a.artist.id.clone()).collect())
                        .unwrap_or_default(),
                    release_mbid: recording
                        .releases
                        .as_ref()
                        .and_then(|r| r.first())
                        .map(|r| r.id.clone()),
                    isrcs: recording.isrcs.clone().unwrap_or_default(),
                    confidence,
                }));
            }
        }

        // TODO: Implement text-based search fallback
        // For now, return None if ISRC search fails
        let _ = (artist, title); // Silence unused warnings
        Ok(None)
    }

    /// The quoted-name search window as cacheable candidates.
    pub async fn search_artist_candidates(
        &self,
        name: &str,
    ) -> IntegrationResult<Vec<identity::ArtistCandidate>> {
        let response = self
            .search_artist(name, identity::NAME_SEARCH_LIMIT)
            .await?;
        Ok(response.artists.iter().map(identity::ArtistCandidate::from).collect())
    }

    /// Resolve an artist NAME to a MusicBrainz id under the containment rule
    /// (`identity::select_exact_name`): exactly one result in the quoted-name
    /// window with the same trimmed, case-insensitive name and score >= 90.
    /// Zero or several acceptable candidates resolve to `None`. There is no
    /// first-result fallback any more — that fallback is how #768 handed
    /// Swim Deep the biography of Deep Purple.
    pub async fn resolve_artist(&self, name: &str) -> IntegrationResult<Option<ResolvedArtist>> {
        let window = self.search_artist_candidates(name).await?;
        Ok(match identity::select_exact_name(name, &window) {
            identity::ExactNameSelection::Unique(c) => Some(c.to_resolved()),
            _ => None,
        })
    }

    // ============ Extended API Methods ============

    /// Search recordings by title and artist
    pub async fn search_recording(
        &self,
        title: &str,
        artist: &str,
    ) -> IntegrationResult<RecordingSearchResponse> {
        self.check_enabled().await?;

        let base = self.base_url().await;
        let query = format!(
            "recording:\"{}\" AND artist:\"{}\"",
            Self::escape_query(title),
            Self::escape_query(artist)
        );
        let url = format!(
            "{}/recording?query={}&fmt=json&limit=5",
            base,
            urlencoding::encode(&query)
        );

        let response = self.send_with_retry(&url).await?;
        self.check_response(&response).await;
        response.json().await.map_err(Into::into)
    }

    /// Get artist details with relationships and tags
    pub async fn get_artist_with_relations(
        &self,
        mbid: &str,
    ) -> IntegrationResult<ArtistFullResponse> {
        self.check_enabled().await?;

        let base = self.base_url().await;
        let url = format!("{}/artist/{}?inc=artist-rels+tags&fmt=json", base, mbid);

        let response = self.send_with_retry(&url).await?;
        response.json().await.map_err(Into::into)
    }

    /// Fetch artist tags only (lightweight, no relations)
    pub async fn get_artist_tags(&self, mbid: &str) -> IntegrationResult<Vec<String>> {
        self.check_enabled().await?;
        let base = self.base_url().await;
        let url = format!("{}/artist/{}?inc=tags&fmt=json", base, mbid);

        // A missing artist or no tags is normal, not an error: swallow any
        // non-2xx (including an exhausted retry) as an empty list, but still go
        // through the retrying sender so a transient 503 is retried first.
        let response = match self.send_with_retry(&url).await {
            Ok(response) => response,
            Err(_) => return Ok(Vec::new()),
        };

        let artist: ArtistFullResponse = response.json().await.map_err(|e| {
            IntegrationError::internal(format!("Failed to parse MusicBrainz response: {}", e))
        })?;

        let mut tags: Vec<_> = artist
            .tags
            .unwrap_or_default()
            .into_iter()
            .filter(|tag| tag.count.unwrap_or(0) > 0)
            .collect();
        tags.sort_by(|a, b| b.count.unwrap_or(0).cmp(&a.count.unwrap_or(0)));
        Ok(tags
            .into_iter()
            .map(|tag| tag.name.to_lowercase())
            .collect())
    }

    /// MBID -> ISRCs bridge. Looks up a recording's ISRCs (the strong key for Qobuz matching).
    /// GET {base}/recording/{recording_mbid}?inc=isrcs&fmt=json
    /// Returns the ISRC list, or an EMPTY vec on any non-success/parse failure (a missing ISRC is normal, not an error).
    pub async fn get_recording_isrcs(&self, recording_mbid: &str) -> IntegrationResult<Vec<String>> {
        self.check_enabled().await?;
        let base = self.base_url().await;
        let url = format!("{}/recording/{}?inc=isrcs&fmt=json", base, recording_mbid);
        // Empty on any failure (a missing ISRC is normal); still retries 503.
        let response = match self.send_with_retry(&url).await {
            Ok(response) => response,
            Err(_) => return Ok(Vec::new()),
        };
        let parsed: RecordingLookupResponse = match response.json().await {
            Ok(p) => p,
            Err(_) => return Ok(Vec::new()),
        };
        Ok(parsed.isrcs.unwrap_or_default())
    }

    /// Search artists by tag (genre)
    pub async fn search_artists_by_tag(
        &self,
        tag: &str,
        limit: usize,
    ) -> IntegrationResult<ArtistSearchResponse> {
        self.check_enabled().await?;

        let base = self.base_url().await;
        let limit = limit.min(100).max(1);
        let query = format!("tag:\"{}\"", Self::escape_query(tag));
        let url = format!(
            "{}/artist?query={}&fmt=json&limit={}",
            base,
            urlencoding::encode(&query),
            limit
        );

        let response = self.send_with_retry(&url).await?;
        response.json().await.map_err(Into::into)
    }

    /// Search artists by tag AND area
    pub async fn search_artists_by_tag_and_area(
        &self,
        tag: &str,
        area_name: &str,
        country: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> IntegrationResult<ArtistSearchResponse> {
        self.check_enabled().await?;

        let base = self.base_url().await;
        let limit = limit.min(100).max(1);
        let search_area = country.unwrap_or(area_name);
        let query = format!(
            "tag:\"{}\" AND area:\"{}\"",
            Self::escape_query(tag),
            Self::escape_query(search_area)
        );
        let url = format!(
            "{}/artist?query={}&fmt=json&limit={}&offset={}",
            base,
            urlencoding::encode(&query),
            limit,
            offset
        );

        let response = self.send_with_retry(&url).await?;
        response.json().await.map_err(Into::into)
    }

    /// Search releases by barcode (UPC/EAN)
    pub async fn search_release_by_barcode(
        &self,
        barcode: &str,
    ) -> IntegrationResult<ReleaseSearchResponse> {
        self.check_enabled().await?;

        let base = self.base_url().await;
        let url = format!(
            "{}/release?query=barcode:{}&fmt=json&limit=5",
            base, barcode
        );

        let response = self.send_with_retry(&url).await?;
        response.json().await.map_err(Into::into)
    }

    /// Search releases by title and artist
    pub async fn search_release(
        &self,
        title: &str,
        artist: &str,
    ) -> IntegrationResult<ReleaseSearchResponse> {
        self.search_releases_extended(title, artist, None, 5).await
    }

    /// Search releases with extended options
    pub async fn search_releases_extended(
        &self,
        title: &str,
        artist: &str,
        catalog_number: Option<&str>,
        limit: usize,
    ) -> IntegrationResult<ReleaseSearchResponse> {
        self.check_enabled().await?;

        let base = self.base_url().await;
        let query = if let Some(catno) = catalog_number.filter(|s| !s.trim().is_empty()) {
            format!(
                "catno:\"{}\" AND artist:\"{}\"",
                Self::escape_query(catno),
                Self::escape_query(artist)
            )
        } else {
            format!(
                "release:\"{}\" AND artist:\"{}\"",
                Self::escape_query(title),
                Self::escape_query(artist)
            )
        };

        let limit = limit.min(25).max(1);
        let url = format!(
            "{}/release?query={}&fmt=json&limit={}",
            base,
            urlencoding::encode(&query),
            limit
        );

        let response = self.send_with_retry(&url).await?;
        response.json().await.map_err(Into::into)
    }

    /// Get full release details including tracks
    pub async fn get_release_with_tracks(
        &self,
        release_id: &str,
    ) -> IntegrationResult<ReleaseFullResponse> {
        self.check_enabled().await?;

        let base = self.base_url().await;
        let url = format!(
            "{}/release/{}?inc=recordings+artist-credits+labels+tags&fmt=json",
            base, release_id
        );

        let response = self.send_with_retry(&url).await?;
        response.json().await.map_err(Into::into)
    }

    /// Browse artists by area MBID
    pub async fn browse_artists_by_area(
        &self,
        area_id: &str,
        limit: usize,
        offset: usize,
    ) -> IntegrationResult<ArtistBrowseResponse> {
        self.check_enabled().await?;

        let base = self.base_url().await;
        let limit = limit.min(100).max(1);
        let url = format!(
            "{}/artist?area={}&fmt=json&limit={}&offset={}&inc=tags",
            base, area_id, limit, offset
        );

        let response = self.send_with_retry(&url).await?;
        response.json().await.map_err(Into::into)
    }

    /// Search for an area by name
    pub async fn search_area(
        &self,
        name: &str,
        area_type: Option<&str>,
    ) -> IntegrationResult<AreaSearchResponse> {
        self.check_enabled().await?;

        let base = self.base_url().await;
        let query = if let Some(atype) = area_type {
            format!(
                "area:\"{}\" AND type:\"{}\"",
                Self::escape_query(name),
                Self::escape_query(atype)
            )
        } else {
            format!("area:\"{}\"", Self::escape_query(name))
        };

        let url = format!(
            "{}/area?query={}&fmt=json&limit=5",
            base,
            urlencoding::encode(&query)
        );

        let response = self.send_with_retry(&url).await?;
        response.json().await.map_err(Into::into)
    }

    /// Look up an area and its parent relationships
    pub async fn get_area_with_relations(
        &self,
        area_id: &str,
    ) -> IntegrationResult<AreaDetailResponse> {
        self.check_enabled().await?;

        let base = self.base_url().await;
        let url = format!("{}/area/{}?inc=area-rels&fmt=json", base, area_id);

        let response = self.send_with_retry(&url).await?;
        response.json().await.map_err(Into::into)
    }

    /// Resolve a city area to its parent subdivision (state/region)
    pub async fn resolve_parent_subdivision(
        &self,
        area_id: &str,
    ) -> IntegrationResult<Option<(String, String)>> {
        let mut current_id = area_id.to_string();
        let mut path: Vec<String> = Vec::new();
        let max_hops = 5;

        for _hop in 0..max_hops {
            let detail = self.get_area_with_relations(&current_id).await?;
            path.push(format!("{}[{:?}]", detail.name, detail.area_type));

            let parents: Vec<_> = detail
                .relations
                .as_ref()
                .map(|rels| {
                    rels.iter()
                        .filter(|rel| {
                            rel.relation_type == "part of"
                                && rel.direction.as_deref() == Some("backward")
                        })
                        .filter_map(|rel| rel.area.as_ref())
                        .collect()
                })
                .unwrap_or_default();

            if parents.is_empty() {
                return Ok(None);
            }

            let has_country_parent = parents.iter().any(|p| {
                p.area_type
                    .as_deref()
                    .map(|t| t.eq_ignore_ascii_case("country"))
                    .unwrap_or(false)
            });

            if has_country_parent {
                let own_type = detail.area_type.as_deref().unwrap_or("");
                if own_type.eq_ignore_ascii_case("subdivision") {
                    if current_id == area_id {
                        return Ok(None);
                    }
                    return Ok(Some((detail.name.clone(), detail.id.clone())));
                }
                if current_id == area_id {
                    return Ok(None);
                }
                return Ok(Some((detail.name.clone(), detail.id.clone())));
            }

            let next = parents
                .iter()
                .find(|p| {
                    p.area_type
                        .as_deref()
                        .map(|t| t.eq_ignore_ascii_case("subdivision"))
                        .unwrap_or(false)
                })
                .or_else(|| {
                    parents.iter().find(|p| {
                        let t = p.area_type.as_deref().unwrap_or("");
                        !t.eq_ignore_ascii_case("city") && !t.eq_ignore_ascii_case("country")
                    })
                })
                .or_else(|| parents.first());

            match next {
                Some(parent) => {
                    current_id = parent.id.clone();
                }
                None => {
                    return Ok(None);
                }
            }
        }

        Ok(None)
    }

    /// Resolve the country that an area belongs to by walking up the hierarchy.
    ///
    /// Frankfurt am Main → Hessen → Germany → returns ("Germany", Some("de"))
    /// Monterrey → Nuevo León → Mexico → returns ("Mexico", Some("mx"))
    ///
    /// Returns (country_name, country_code_lowercase) or None.
    pub async fn resolve_area_country(
        &self,
        area_id: &str,
    ) -> IntegrationResult<Option<(String, Option<String>)>> {
        let mut current_id = area_id.to_string();
        let max_hops = 5;

        for _hop in 0..max_hops {
            let detail = self.get_area_with_relations(&current_id).await?;

            // If current area IS a country, return it
            if detail
                .area_type
                .as_deref()
                .map(|t| t.eq_ignore_ascii_case("country"))
                .unwrap_or(false)
            {
                let code = detail
                    .iso_codes
                    .as_ref()
                    .and_then(|c| c.first())
                    .map(|c| c.to_lowercase());
                return Ok(Some((detail.name, code)));
            }

            // Find "part of" parent
            let parent = detail
                .relations
                .as_ref()
                .and_then(|rels| {
                    rels.iter()
                        .find(|rel| {
                            rel.relation_type == "part of"
                                && rel.direction.as_deref() == Some("backward")
                        })
                        .and_then(|rel| rel.area.as_ref())
                });

            match parent {
                Some(p) => {
                    if p.area_type
                        .as_deref()
                        .map(|t| t.eq_ignore_ascii_case("country"))
                        .unwrap_or(false)
                    {
                        let code = p
                            .iso_codes
                            .as_ref()
                            .and_then(|c| c.first())
                            .map(|c| c.to_lowercase());
                        return Ok(Some((p.name.clone(), code)));
                    }
                    current_id = p.id.clone();
                }
                None => return Ok(None),
            }
        }

        Ok(None)
    }

    // ============ Internal Helpers ============

    async fn check_enabled(&self) -> IntegrationResult<()> {
        if !self.is_enabled().await {
            return Err(IntegrationError::ServiceUnavailable(
                "MusicBrainz integration is disabled".into(),
            ));
        }
        Ok(())
    }

    #[allow(unused)]
    async fn check_response(&self, _response: &reqwest::Response) {
        // Placeholder for response logging/metrics
    }

    /// GET `url` behind the shared rate limiter, retrying MusicBrainz's 503
    /// (or the proxy's translated 429) with the server's `Retry-After` capped at
    /// 8s, up to 3 total attempts. The limiter waits again before EVERY attempt,
    /// so a retry never jumps the 1.1s spacing. Callers get exactly the response
    /// or error `handle_response_status` produced; the only new behaviour is
    /// that a transient rate-limit is absorbed here instead of surfacing on the
    /// first try. This is the ONE path every endpoint GET goes through.
    async fn send_with_retry(&self, url: &str) -> IntegrationResult<reqwest::Response> {
        const MAX_ATTEMPTS: u32 = 3;
        // Transport failures cost a full request timeout each, so cap them lower:
        // 2 attempts * 6s timeout + 1 backoff = ~13s worst case (owner's target),
        // while a hang that clears still recovers on the second try.
        const TRANSPORT_MAX_ATTEMPTS: u32 = 2;
        const BACKOFF_CAP_SECS: u64 = 8;
        const TRANSPORT_BACKOFF_SECS: u64 = 1;
        let mut attempt = 0;
        loop {
            attempt += 1;
            self.rate_limiter.wait().await;
            let response = match self.client.get(url).send().await {
                Ok(response) => response,
                // A `send()` failure means NO response arrived: MusicBrainz
                // hanging until our request timeout, or the connection dropped /
                // refused under load. That is the 503's quieter cousin (the
                // owner hit exactly this -- "error sending request for url" on
                // `artist?query=`), so retry it on the same budget. A builder
                // error (a malformed URL) is not transient and is surfaced.
                Err(e) if attempt < TRANSPORT_MAX_ATTEMPTS && !e.is_builder() => {
                    log::warn!(
                        "[musicbrainz] transport error (attempt {attempt}/{MAX_ATTEMPTS}, \
                         timeout={}, connect={}), retrying in {TRANSPORT_BACKOFF_SECS}s: {e}",
                        e.is_timeout(),
                        e.is_connect()
                    );
                    tokio::time::sleep(Duration::from_secs(TRANSPORT_BACKOFF_SECS)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            Self::log_rate_headers(&response);
            match self.handle_response_status(response).await {
                Ok(ok) => return Ok(ok),
                Err(IntegrationError::RateLimited(secs)) if attempt < MAX_ATTEMPTS => {
                    let backoff = secs.min(BACKOFF_CAP_SECS);
                    log::warn!(
                        "[musicbrainz] rate limited (attempt {attempt}/{MAX_ATTEMPTS}), \
                         backing off {backoff}s then retrying: {url}"
                    );
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Log MusicBrainz's `x-ratelimit-*` budget headers — `debug` on success,
    /// `warn` alongside a 429/503 — because they are the only signal that says
    /// whether the exhausted bucket was our own IP or MB's global pool.
    /// Diagnostics only; no metrics.
    fn log_rate_headers(response: &reqwest::Response) {
        let status = response.status();
        if !status.is_success() && !matches!(status.as_u16(), 429 | 503) {
            return;
        }
        let headers = response.headers();
        let value = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
        };
        let limit = value("x-ratelimit-limit");
        let remaining = value("x-ratelimit-remaining");
        let reset = value("x-ratelimit-reset");
        if status.is_success() {
            log::debug!(
                "[musicbrainz] {} x-ratelimit limit={limit} remaining={remaining} reset={reset}",
                status.as_u16()
            );
        } else {
            log::warn!(
                "[musicbrainz] {} rate limited -- x-ratelimit limit={limit} remaining={remaining} reset={reset}",
                status.as_u16()
            );
        }
    }

    async fn handle_response_status(
        &self,
        response: reqwest::Response,
    ) -> IntegrationResult<reqwest::Response> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        // 429 (proxy-translated) and 503 (direct MusicBrainz) both signal that
        // the rate limit was hit. Surface the server's Retry-After so the caller
        // can back off instead of treating it as a generic error.
        if matches!(status.as_u16(), 429 | 503) {
            return Err(IntegrationError::RateLimited(Self::parse_retry_after(
                &response,
            )));
        }
        let text = response.text().await.unwrap_or_default();
        Err(IntegrationError::internal(format!(
            "MusicBrainz API error {}: {}",
            status, text
        )))
    }

    /// Parse the `Retry-After` header (whole seconds). MusicBrainz sends it on
    /// HTTP 503 when the per-IP rate limit is exceeded. Defaults to 1s because
    /// MB's per-IP limiter recovers within ~1 second.
    fn parse_retry_after(response: &reqwest::Response) -> u64 {
        response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|&s| s > 0)
            .unwrap_or(1)
    }

    /// Escape special characters in Lucene queries
    fn escape_query(s: &str) -> String {
        identity::lucene_escape(s)
    }
}

impl identity::IdentityFetcher for MusicBrainzClient {
    fn artist_candidates<'a>(
        &'a self,
        name: &'a str,
    ) -> identity::BoxFuture<'a, Result<Vec<identity::ArtistCandidate>, identity::FetchError>> {
        Box::pin(async move {
            self.search_artist_candidates(name)
                .await
                .map_err(|e| identity::FetchError::Unavailable(e.to_string()))
        })
    }

    fn isrc_credits<'a>(
        &'a self,
        isrc: &'a str,
    ) -> identity::BoxFuture<'a, Result<identity::IsrcCredits, identity::FetchError>> {
        Box::pin(async move {
            let response = self
                .search_recording_by_isrc(isrc)
                .await
                .map_err(|e| identity::FetchError::Unavailable(e.to_string()))?;
            Ok(identity::IsrcCredits::from_search(isrc, &response))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// reqwest is built with `rustls-no-provider`; the app installs
    /// aws-lc-rs at startup (qbz_app::ensure_crypto_provider). Mirror it,
    /// idempotently, for any test that builds a client.
    fn ensure_provider() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }

    /// Piece 1: a 503 carrying `Retry-After: 1` followed by a 200 must be
    /// absorbed inside `send_with_retry` — a single caller `await` returns Ok,
    /// never the RateLimited error.
    #[tokio::test]
    async fn send_with_retry_absorbs_a_503_then_succeeds() {
        ensure_provider();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let body = "{\"ok\":true}";
            let replies = [
                "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                ),
            ];
            for reply in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await.unwrap();
                socket.write_all(reply.as_bytes()).await.unwrap();
                socket.flush().await.unwrap();
            }
        });

        let client = MusicBrainzClient::new();
        let url = format!("http://{addr}/artist");
        let response = client
            .send_with_retry(&url)
            .await
            .expect("a 503 then 200 must resolve to Ok");
        assert!(response.status().is_success());
        let parsed: serde_json::Value = response.json().await.unwrap();
        assert_eq!(parsed["ok"], serde_json::json!(true));
        server.await.unwrap();
    }

    /// Piece 2: every client shares ONE process-wide direct limiter, so two
    /// clients issuing back-to-back waits serialize (>= the 1.1s interval). With
    /// a per-client limiter both waits would be immediate.
    #[tokio::test]
    async fn clients_share_one_rate_limiter() {
        ensure_provider();
        let a = MusicBrainzClient::new();
        let b = MusicBrainzClient::new();
        let start = Instant::now();
        a.rate_limiter.wait().await;
        b.rate_limiter.wait().await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(1000),
            "two clients on a shared limiter must serialize (>=~1.1s), took {}ms -- not shared?",
            elapsed.as_millis()
        );
    }

    /// Regression: MusicBrainz under load hangs the connection until our request
    /// timeout, or drops it — reqwest returns a `send()` error, NOT a 503 status.
    /// `send_with_retry` must absorb that transient transport failure too, not
    /// just the rate-limit status. Here the first connection is accepted and
    /// dropped (the client's send fails), the second answers 200.
    #[tokio::test]
    async fn send_with_retry_absorbs_a_dropped_connection() {
        ensure_provider();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            drop(socket); // accept then close -> the client's send() errors
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await.unwrap();
            let body = "{\"ok\":true}";
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
        });
        let client = MusicBrainzClient::new();
        let url = format!("http://{addr}/artist");
        let response = client
            .send_with_retry(&url)
            .await
            .expect("a dropped connection must be retried, not surfaced");
        assert!(response.status().is_success());
        server.await.unwrap();
    }
}
