//! Self-contained Odesli/song.link client.
//!
//! Ported from `src-tauri/src/share/{songlink,models,errors}.rs`. This is a
//! frontend-agnostic copy so the resolver does not depend on the Tauri `share`
//! module. The Odesli endpoint is `https://api.song.link/v1-alpha.1/links`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const ODESLI_API_URL: &str = "https://api.song.link/v1-alpha.1/links";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CACHE_TTL: Duration = Duration::from_secs(3600); // 1 hour

// ── Error type ──

#[derive(Error, Debug)]
pub enum ShareError {
    #[error("Network error: {0}")]
    NetworkError(#[from] reqwest::Error),

    #[error("Odesli API error: {0}")]
    OdesliError(String),

    #[error("No matches found on Odesli")]
    NoMatches,
}

// ── Models ──

/// Response from Odesli API
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // wire-shape fields kept for fidelity; not all are read here
pub struct OdesliResponse {
    /// The unique ID for the input entity that was supplied in the request
    pub entity_unique_id: Option<String>,

    /// The userCountry query param that was supplied in the request
    pub user_country: Option<String>,

    /// The main song.link page URL
    pub page_url: String,

    /// A map of platform names to their link info
    #[serde(default)]
    pub links_by_platform: HashMap<String, PlatformLink>,

    /// A map of entity unique IDs to entity info
    #[serde(default)]
    pub entities_by_unique_id: HashMap<String, Entity>,
}

/// Link info for a specific platform
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformLink {
    /// The country code for this link
    pub country: Option<String>,

    /// The URL to this entity on the platform
    pub url: String,

    /// The native app URI
    pub native_app_uri_mobile: Option<String>,
    pub native_app_uri_desktop: Option<String>,

    /// The unique ID for this entity
    pub entity_unique_id: Option<String>,
}

/// Entity info from Odesli
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // wire-shape fields kept for fidelity; not all are read here
pub struct Entity {
    /// The unique ID (can be a string or number in the API response)
    #[serde(deserialize_with = "deserialize_string_or_number")]
    pub id: String,

    /// Type: "song", "album"
    #[serde(rename = "type")]
    pub entity_type: Option<String>,

    /// Title
    pub title: Option<String>,

    /// Artist name
    pub artist_name: Option<String>,

    /// Thumbnail URL
    pub thumbnail_url: Option<String>,

    /// Thumbnail dimensions
    pub thumbnail_width: Option<u32>,
    pub thumbnail_height: Option<u32>,

    /// API provider
    pub api_provider: Option<String>,

    /// Platforms this entity is available on
    pub platforms: Option<Vec<String>>,
}

/// Simplified response for consumption
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SongLinkResponse {
    /// The main song.link URL to share
    pub page_url: String,

    /// Title of the content (if available)
    pub title: Option<String>,

    /// Artist name (if available)
    pub artist: Option<String>,

    /// Thumbnail URL (if available)
    pub thumbnail_url: Option<String>,

    /// Map of platform names to their direct URLs
    pub platforms: HashMap<String, String>,

    /// The identifier used (ISRC or UPC)
    pub identifier: String,

    /// Type of content: "track" or "album"
    pub content_type: String,
}

/// Content type for sharing
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Track,
    Album,
}

impl ContentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ContentType::Track => "track",
            ContentType::Album => "album",
        }
    }
}

/// Deserialize a JSON value that may be a string or a number into a String.
/// Bandcamp's Odesli entities return numeric IDs while others return strings.
fn deserialize_string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct StringOrNumber;

    impl<'de> de::Visitor<'de> for StringOrNumber {
        type Value = String;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string or number")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<String, E> {
            Ok(v.to_string())
        }
    }

    deserializer.deserialize_any(StringOrNumber)
}

// ── Client ──

/// Cached entry with TTL
struct CacheEntry {
    response: SongLinkResponse,
    created_at: Instant,
}

impl CacheEntry {
    fn is_expired(&self) -> bool {
        self.created_at.elapsed() > CACHE_TTL
    }
}

/// Odesli/song.link client with caching
pub struct SongLinkClient {
    client: Client,
    cache: Mutex<HashMap<String, CacheEntry>>,
}

impl Default for SongLinkClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SongLinkClient {
    pub fn new() -> Self {
        Self {
            client: qbz_net_proxy::apply_current(Client::builder())
                .unwrap_or_else(|_| Client::builder())
                .timeout(REQUEST_TIMEOUT)
                .connect_timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Get song.link URL by URL (fallback when ISRC/UPC are missing)
    pub async fn get_by_url(
        &self,
        url: &str,
        content_type: ContentType,
    ) -> Result<SongLinkResponse, ShareError> {
        let cache_key = format!("url:{}", url);

        if let Some(cached) = self.get_from_cache(&cache_key) {
            log::debug!("Cache hit for URL: {}", url);
            return Ok(cached);
        }

        log::info!("Fetching song.link for URL: {}", url);

        let response = self
            .client
            .get(ODESLI_API_URL)
            .query(&[("url", url), ("userCountry", "US")])
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            // Provide a friendlier message for common errors
            if status.as_u16() == 400 && text.contains("could_not_resolve_entity") {
                return Err(ShareError::OdesliError(
                    "Track not found on any supported platform. Try a track with an ISRC code."
                        .to_string(),
                ));
            }
            return Err(ShareError::OdesliError(format!(
                "HTTP {}: {}",
                status, text
            )));
        }

        let odesli: OdesliResponse = response.json().await?;
        let result = self.convert_response(odesli, url.to_string(), content_type)?;

        self.store_in_cache(cache_key, result.clone());
        Ok(result)
    }

    /// Convert Odesli response to our simplified format
    fn convert_response(
        &self,
        response: OdesliResponse,
        identifier: String,
        content_type: ContentType,
    ) -> Result<SongLinkResponse, ShareError> {
        // Extract title and artist from the first entity
        let (title, artist, thumbnail_url) = response
            .entities_by_unique_id
            .values()
            .next()
            .map(|e| {
                (
                    e.title.clone(),
                    e.artist_name.clone(),
                    e.thumbnail_url.clone(),
                )
            })
            .unwrap_or((None, None, None));

        // Extract platform URLs
        let platforms: HashMap<String, String> = response
            .links_by_platform
            .into_iter()
            .map(|(platform, link)| (platform, link.url))
            .collect();

        if platforms.is_empty() {
            return Err(ShareError::NoMatches);
        }

        Ok(SongLinkResponse {
            page_url: response.page_url,
            title,
            artist,
            thumbnail_url,
            platforms,
            identifier,
            content_type: content_type.as_str().to_string(),
        })
    }

    /// Get from cache if not expired
    fn get_from_cache(&self, key: &str) -> Option<SongLinkResponse> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(key)?;

        if entry.is_expired() {
            None
        } else {
            Some(entry.response.clone())
        }
    }

    /// Store in cache
    fn store_in_cache(&self, key: String, response: SongLinkResponse) {
        if let Ok(mut cache) = self.cache.lock() {
            // Clean up expired entries occasionally
            if cache.len() > 100 {
                cache.retain(|_, entry| !entry.is_expired());
            }

            cache.insert(
                key,
                CacheEntry {
                    response,
                    created_at: Instant::now(),
                },
            );
        }
    }

    /// Clear the cache
    pub fn clear_cache(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }
}
