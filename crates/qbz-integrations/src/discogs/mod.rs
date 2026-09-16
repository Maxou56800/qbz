//! Discogs API client for fetching album artwork
//!
//! Uses Cloudflare Workers proxy to search the Discogs database and download cover images.

use reqwest::Client;
use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::time::Duration;

// Cloudflare Workers proxy URL - handles credentials
const DISCOGS_PROXY_URL: &str = "https://qbz-api-proxy.blitzkriegfc.workers.dev/discogs";

/// Discogs API client
pub struct DiscogsClient {
    client: Client,
}

/// Search result from Discogs API
#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
}

#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct SearchResult {
    pub id: u64,
    pub cover_image: Option<String>,
    pub thumb: Option<String>,
    pub title: String,
    #[serde(rename = "type")]
    pub result_type: String,
}

/// Image option for artwork selection
#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct DiscogsImageOption {
    pub url: String,
    pub width: u32,
    pub height: u32,
    #[serde(rename = "type")]
    pub image_type: String,
    pub release_title: Option<String>,
    pub release_year: Option<u32>,
}

/// Release details from Discogs API (internal, for artwork)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ReleaseDetails {
    id: u64,
    title: String,
    year: Option<u32>,
    images: Option<Vec<ReleaseImage>>,
}

/// Image from release details
#[derive(Debug, Deserialize)]
struct ReleaseImage {
    uri: String,
    width: u32,
    height: u32,
    #[serde(rename = "type")]
    image_type: String,
}

// ============ Public Metadata Structures ============

/// Full release metadata from Discogs (for tag editor)
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct DiscogsReleaseMetadata {
    pub id: u64,
    pub title: String,
    pub artists: Option<Vec<DiscogsArtist>>,
    pub year: Option<u32>,
    pub genres: Option<Vec<String>>,
    pub styles: Option<Vec<String>>,
    pub labels: Option<Vec<DiscogsLabel>>,
    pub tracklist: Option<Vec<DiscogsTrack>>,
    pub country: Option<String>,
    /// URL to view on Discogs
    pub uri: Option<String>,
}

/// Artist in Discogs release
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct DiscogsArtist {
    pub name: String,
    pub id: Option<u64>,
    /// Join phrase (e.g., " & ", " feat. ")
    pub join: Option<String>,
}

/// Label in Discogs release
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct DiscogsLabel {
    pub name: String,
    pub catno: Option<String>,
    pub id: Option<u64>,
}

/// Track in Discogs release
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct DiscogsTrack {
    /// Position (e.g., "1", "A1", "1-1")
    pub position: String,
    pub title: String,
    /// Duration as string (e.g., "3:45")
    pub duration: Option<String>,
    /// Track type (e.g., "track", "heading")
    #[serde(rename = "type_")]
    pub track_type: Option<String>,
}

/// Extended search result with more metadata
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct DiscogsSearchResultExtended {
    pub id: u64,
    pub title: String,
    #[serde(rename = "type")]
    pub result_type: String,
    pub year: Option<String>,
    pub country: Option<String>,
    pub label: Option<Vec<String>>,
    pub catno: Option<String>,
    pub format: Option<Vec<String>>,
    pub cover_image: Option<String>,
}

impl DiscogsClient {
    /// Create a new Discogs client (proxy handles credentials)
    pub fn new() -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static("QBZ/1.0.0"),
        );

        let client = qbz_net_proxy::apply_current(Client::builder())
            .expect("Failed to apply proxy configuration")
            .timeout(Duration::from_secs(10))
            .default_headers(headers)
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }

    /// Check if credentials are configured (always true - proxy handles credentials)
    pub fn has_credentials(&self) -> bool {
        true
    }

    /// Search for album artwork and download if found
    /// Returns the path to the downloaded image or None
    pub async fn fetch_artwork(
        &self,
        artist: &str,
        album: &str,
        cache_dir: &Path,
    ) -> Option<String> {
        // Search for the release
        let cover_url = self.search_release(artist, album).await?;

        // Generate cache filename
        let filename = format!(
            "discogs_{:x}.jpg",
            Self::simple_hash(&format!("{}_{}", artist, album))
        );
        let cache_path = cache_dir.join(&filename);

        // Return cached if exists
        if cache_path.exists() {
            return Some(cache_path.to_string_lossy().to_string());
        }

        // Download the image
        self.download_image(&cover_url, &cache_path).await?;

        Some(cache_path.to_string_lossy().to_string())
    }

    /// Search for a release and return the cover image URL
    async fn search_release(&self, artist: &str, album: &str) -> Option<String> {
        // Build search query
        let query = format!("{} {}", artist, album);
        let url = format!(
            "{}/search?q={}&type=release",
            DISCOGS_PROXY_URL,
            urlencoding::encode(&query)
        );

        log::debug!("Searching Discogs for: {} - {}", artist, album);

        let response = self.client.get(&url).send().await.ok()?;

        if !response.status().is_success() {
            log::warn!("Discogs search failed with status: {}", response.status());
            return None;
        }

        let search: SearchResponse = response.json().await.ok()?;

        // Find first result with a cover image
        for result in search.results {
            if result.result_type == "release" || result.result_type == "master" {
                if let Some(cover) = result.cover_image {
                    if !cover.is_empty() && !cover.contains("spacer.gif") {
                        log::debug!("Found Discogs cover for {} - {}", artist, album);
                        return Some(cover);
                    }
                }
                // Fallback to thumbnail
                if let Some(thumb) = result.thumb {
                    if !thumb.is_empty() && !thumb.contains("spacer.gif") {
                        return Some(thumb);
                    }
                }
            }
        }

        log::debug!("No Discogs cover found for {} - {}", artist, album);
        None
    }

    /// Get detailed release information including all images
    async fn get_release_details(&self, release_id: u64) -> Result<ReleaseDetails, String> {
        let url = format!("{}/release/{}", DISCOGS_PROXY_URL, release_id);

        log::debug!("Fetching Discogs release details for ID: {}", release_id);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch release details: {}", e))?;

        if !response.status().is_success() {
            return Err(format!(
                "Failed to fetch release details: {}",
                response.status()
            ));
        }

        let details: ReleaseDetails = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse release details: {}", e))?;

        Ok(details)
    }

    /// Search for album artwork options
    /// Returns up to 10 image options, with detailed images from top 2 releases interleaved
    /// If catalog_number is provided, searches by that first, then falls back to artist + album
    pub async fn search_artwork_options(
        &self,
        artist: &str,
        album: &str,
        catalog_number: Option<&str>,
    ) -> Result<Vec<DiscogsImageOption>, String> {
        // Build search query - prefer catalog number if available
        let query = if let Some(catno) = catalog_number.filter(|s| !s.trim().is_empty()) {
            catno.to_string()
        } else {
            format!("{} {}", artist, album)
        };
        let url = format!(
            "{}/search?q={}&type=release",
            DISCOGS_PROXY_URL,
            urlencoding::encode(&query)
        );

        log::debug!(
            "Searching Discogs artwork options for: {} - {} (catalog: {:?})",
            artist,
            album,
            catalog_number
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to search Discogs: {}", e))?;

        if !response.status().is_success() {
            return Err(format!(
                "Discogs search failed with status: {}",
                response.status()
            ));
        }

        let search: SearchResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse Discogs response: {}", e))?;

        // Get IDs of top 2 relevant releases
        let mut release_ids: Vec<u64> = Vec::new();
        let mut other_results: Vec<&SearchResult> = Vec::new();

        for result in search.results.iter().take(20) {
            if result.result_type == "release" || result.result_type == "master" {
                if release_ids.len() < 2 {
                    release_ids.push(result.id);
                } else {
                    other_results.push(result);
                }
            }
        }

        if release_ids.is_empty() {
            return Err("No releases found on Discogs".to_string());
        }

        let mut all_images = Vec::new();
        let mut seen_urls = std::collections::HashSet::new();

        // Fetch detailed images from top 2 releases
        for (idx, release_id) in release_ids.iter().enumerate() {
            match self.get_release_details(*release_id).await {
                Ok(details) => {
                    if let Some(images) = details.images {
                        let mut count = 0;
                        for img in images {
                            if !img.uri.is_empty()
                                && !img.uri.contains("spacer.gif")
                                && seen_urls.insert(img.uri.clone())
                                && count < 4
                            {
                                all_images.push(DiscogsImageOption {
                                    url: img.uri,
                                    width: img.width,
                                    height: img.height,
                                    image_type: img.image_type,
                                    release_title: Some(details.title.clone()),
                                    release_year: details.year,
                                });
                                count += 1;
                            }
                        }
                        log::debug!(
                            "Added {} images from release #{} ({})",
                            count,
                            idx + 1,
                            details.title
                        );
                    }
                }
                Err(e) => {
                    log::warn!("Failed to fetch details for release {}: {}", release_id, e);
                }
            }
        }

        // Add up to 2 more images from other search results
        for result in other_results.iter().take(10) {
            if all_images.len() >= 10 {
                break;
            }

            // Prefer cover image
            let image_url = if let Some(cover) = &result.cover_image {
                if !cover.is_empty() && !cover.contains("spacer.gif") {
                    Some((cover.clone(), 600, 600, "primary".to_string()))
                } else {
                    None
                }
            } else {
                None
            };

            let image_url = image_url.or_else(|| {
                result.thumb.as_ref().and_then(|thumb| {
                    if !thumb.is_empty() && !thumb.contains("spacer.gif") {
                        Some((thumb.clone(), 150, 150, "secondary".to_string()))
                    } else {
                        None
                    }
                })
            });

            if let Some((url, width, height, img_type)) = image_url {
                if seen_urls.insert(url.clone()) {
                    all_images.push(DiscogsImageOption {
                        url,
                        width,
                        height,
                        image_type: img_type,
                        release_title: Some(result.title.clone()),
                        release_year: None,
                    });
                }
            }
        }

        if all_images.is_empty() {
            return Err("No artwork found on Discogs".to_string());
        }

        // Return up to 10 unique images
        all_images.truncate(10);
        log::info!(
            "Returning {} artwork options from Discogs",
            all_images.len()
        );
        Ok(all_images)
    }

    /// Download image from URL and return local path
    pub async fn download_artwork_from_url(
        &self,
        image_url: &str,
        cache_dir: &Path,
        artist: &str,
        album: &str,
    ) -> Result<String, String> {
        // Generate cache filename
        let filename = format!(
            "discogs_{:x}.jpg",
            Self::simple_hash(&format!("{}_{}", artist, album))
        );
        let cache_path = cache_dir.join(&filename);

        // Download the image
        self.download_image(image_url, &cache_path)
            .await
            .ok_or_else(|| "Failed to download image".to_string())?;

        Ok(cache_path.to_string_lossy().to_string())
    }

    /// Download one previously returned Discogs image without trusting its
    /// size or exposing the proxy URL to a frontend. The editor stages these
    /// bytes and validates the decoded image before any tag/file mutation.
    pub async fn download_artwork_bytes(
        &self,
        image_url: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, String> {
        let proxy_url = format!(
            "{}/image?url={}",
            DISCOGS_PROXY_URL,
            urlencoding::encode(image_url)
        );
        let mut response = self
            .client
            .get(&proxy_url)
            .send()
            .await
            .map_err(|error| format!("Failed to download Discogs image: {error}"))?
            .error_for_status()
            .map_err(|error| format!("Discogs image request failed: {error}"))?;
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes as u64)
        {
            return Err("Discogs image is larger than the editor limit".to_string());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("Failed to read Discogs image: {error}"))?
        {
            if bytes.len().saturating_add(chunk.len()) > max_bytes {
                return Err("Discogs image is larger than the editor limit".to_string());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    /// Search for artists and return search results
    pub async fn search_artist(&self, query: &str) -> Result<SearchResponse, String> {
        let url = format!(
            "{}/search?q={}&type=artist",
            DISCOGS_PROXY_URL,
            urlencoding::encode(query)
        );

        log::debug!("Searching Discogs for artist: {}", query);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to search Discogs: {}", e))?;

        if !response.status().is_success() {
            return Err(format!(
                "Discogs search failed with status: {}",
                response.status()
            ));
        }

        let search: SearchResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse Discogs response: {}", e))?;

        Ok(search)
    }

    /// Search for releases with extended metadata
    /// Returns up to `limit` results with detailed release information
    pub async fn search_releases(
        &self,
        artist: &str,
        album: &str,
        catalog_number: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DiscogsSearchResultExtended>, String> {
        // Build search query - prefer catalog number if available
        let query = if let Some(catno) = catalog_number.filter(|s| !s.trim().is_empty()) {
            catno.to_string()
        } else {
            format!("{} {}", artist, album)
        };

        let url = format!(
            "{}/search?q={}&type=release&per_page={}",
            DISCOGS_PROXY_URL,
            urlencoding::encode(&query),
            limit.min(25)
        );

        log::debug!(
            "Searching Discogs releases for: {} - {} (catalog: {:?})",
            artist,
            album,
            catalog_number
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to search Discogs: {}", e))?;

        if !response.status().is_success() {
            return Err(format!(
                "Discogs search failed with status: {}",
                response.status()
            ));
        }

        // Parse response with extended fields
        #[derive(Debug, Deserialize)]
        struct ExtendedSearchResponse {
            results: Vec<DiscogsSearchResultExtended>,
        }

        let search: ExtendedSearchResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse Discogs response: {}", e))?;

        // Filter to releases only
        let results: Vec<_> = search
            .results
            .into_iter()
            .filter(|r| r.result_type == "release" || r.result_type == "master")
            .collect();

        log::info!("Found {} Discogs releases", results.len());
        Ok(results)
    }

    /// Get full release metadata including tracklist
    pub async fn get_release_metadata(
        &self,
        release_id: u64,
    ) -> Result<DiscogsReleaseMetadata, String> {
        let url = format!("{}/release/{}", DISCOGS_PROXY_URL, release_id);

        log::debug!("Fetching Discogs release metadata for ID: {}", release_id);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch release: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Failed to fetch release: {}", response.status()));
        }

        let metadata: DiscogsReleaseMetadata = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse release metadata: {}", e))?;

        log::info!(
            "Fetched Discogs release: {} ({:?})",
            metadata.title,
            metadata.year
        );
        Ok(metadata)
    }

    /// Download an image to the cache directory
    async fn download_image(&self, image_url: &str, path: &Path) -> Option<()> {
        log::debug!("Downloading Discogs artwork: {}", image_url);

        // Use proxy to download image with authentication
        let proxy_url = format!(
            "{}/image?url={}",
            DISCOGS_PROXY_URL,
            urlencoding::encode(image_url)
        );

        let response = self.client.get(&proxy_url).send().await.ok()?;

        if !response.status().is_success() {
            log::warn!("Failed to download Discogs image: {}", response.status());
            return None;
        }

        let bytes = response.bytes().await.ok()?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok()?;
        }

        fs::write(path, &bytes).ok()?;

        log::info!("Saved Discogs artwork to: {}", path.display());
        Some(())
    }

    /// Simple hash function for generating filenames
    fn simple_hash(s: &str) -> u64 {
        let mut hash: u64 = 5381;
        for byte in s.bytes() {
            hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
        }
        hash
    }
}

impl Default for DiscogsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash() {
        let hash1 = DiscogsClient::simple_hash("Artist_Album");
        let hash2 = DiscogsClient::simple_hash("Artist_Album");
        assert_eq!(hash1, hash2);

        let hash3 = DiscogsClient::simple_hash("Different_Album");
        assert_ne!(hash1, hash3);
    }
}
