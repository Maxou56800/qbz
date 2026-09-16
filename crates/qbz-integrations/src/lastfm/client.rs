//! Last.fm API client

use reqwest::Client;
use serde_json::json;

use super::models::{
    AuthGetSessionResponse, AuthGetTokenResponse, LastFmAlbum, LastFmArtist, LastFmResponse,
    LastFmSession, LastFmSimilarArtist, LastFmSimilarTrack, LastFmTrack,
};
use crate::error::{IntegrationError, IntegrationResult};

/// Cloudflare Workers proxy URL - handles API credentials and signature generation
const LASTFM_PROXY_URL: &str = "https://qbz-api-proxy.blitzkriegfc.workers.dev/lastfm";

/// Last.fm API client
///
/// Uses Cloudflare Workers proxy to handle API credentials and signature generation.
/// This means the client doesn't need to know the API key or secret.
pub struct LastFmClient {
    client: Client,
    session_key: Option<String>,
}

impl Default for LastFmClient {
    fn default() -> Self {
        Self::new()
    }
}

impl LastFmClient {
    /// Create a new Last.fm client
    pub fn new() -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static("QBZ/1.0.0"),
        );

        let builder =
            qbz_net_proxy::apply_current(Client::builder()).unwrap_or_else(|_| Client::builder());
        Self {
            client: builder
                .default_headers(headers)
                .build()
                .unwrap_or_else(|_| Client::new()),
            session_key: None,
        }
    }

    /// Create a client with an existing session key
    pub fn with_session_key(session_key: String) -> Self {
        let mut client = Self::new();
        client.session_key = Some(session_key);
        client
    }

    /// Set the session key (for restoring a saved session)
    pub fn set_session_key(&mut self, key: String) {
        self.session_key = Some(key);
    }

    /// Get the current session key
    pub fn session_key(&self) -> Option<&str> {
        self.session_key.as_deref()
    }

    /// Check if authenticated
    pub fn is_authenticated(&self) -> bool {
        self.session_key.is_some()
    }

    /// Clear the session (logout)
    pub fn clear_session(&mut self) {
        self.session_key = None;
    }

    /// Get a request token and authorization URL for authentication
    ///
    /// Returns: (token, auth_url)
    ///
    /// The user should be directed to auth_url to authorize the application.
    /// Once authorized, call `get_session` with the token to complete authentication.
    pub async fn get_token(&self) -> IntegrationResult<(String, String)> {
        let url = format!("{}/auth.getToken", LASTFM_PROXY_URL);

        let response = self.client.post(&url).json(&json!({})).send().await?;

        let data: LastFmResponse<AuthGetTokenResponse> = response.json().await?;

        match data {
            LastFmResponse::Success(r) => {
                let auth_url = r
                    .auth_url
                    .unwrap_or_else(|| format!("https://www.last.fm/api/auth/?token={}", r.token));
                Ok((r.token, auth_url))
            }
            LastFmResponse::Error { error, message } => Err(IntegrationError::api(error, message)),
        }
    }

    /// Get session key after user has authorized
    ///
    /// Call this after the user has visited the auth_url from `get_token`.
    pub async fn get_session(&mut self, token: &str) -> IntegrationResult<LastFmSession> {
        // Never log request-token material (even a prefix) — support log
        // bundles must not carry auth substrings.
        log::info!("Requesting Last.fm session");

        let url = format!("{}/auth.getSession", LASTFM_PROXY_URL);

        let response = self
            .client
            .post(&url)
            .json(&json!({ "token": token }))
            .send()
            .await?;

        let response_text = response.text().await?;
        // Do not log the raw body: it includes the session key.
        let data: LastFmResponse<AuthGetSessionResponse> = serde_json::from_str(&response_text)?;

        match data {
            LastFmResponse::Success(r) => {
                log::info!("Last.fm session obtained for user: {}", r.session.name);
                self.session_key = Some(r.session.key.clone());
                Ok(r.session)
            }
            LastFmResponse::Error { error, message } => {
                log::error!("Last.fm auth error {}: {}", error, message);
                Err(IntegrationError::api(error, message))
            }
        }
    }

    /// Scrobble a track (mark as played)
    ///
    /// Requires authentication.
    pub async fn scrobble(
        &self,
        artist: &str,
        track: &str,
        album: Option<&str>,
        timestamp: u64,
    ) -> IntegrationResult<()> {
        let session_key = self
            .session_key
            .as_ref()
            .ok_or(IntegrationError::NotAuthenticated)?;

        let url = format!("{}/track.scrobble", LASTFM_PROXY_URL);

        let mut body = json!({
            "sk": session_key,
            "artist": artist,
            "track": track,
            "timestamp": timestamp.to_string(),
        });

        if let Some(album_name) = album {
            body["album"] = json!(album_name);
        }

        let response = self.client.post(&url).json(&body).send().await?;

        if response.status().is_success() {
            log::info!("Scrobbled: {} - {}", artist, track);
            Ok(())
        } else {
            let text = response.text().await.unwrap_or_default();
            Err(IntegrationError::internal(format!(
                "Scrobble failed: {}",
                text
            )))
        }
    }

    /// Update "now playing" status
    ///
    /// Requires authentication.
    pub async fn update_now_playing(
        &self,
        artist: &str,
        track: &str,
        album: Option<&str>,
    ) -> IntegrationResult<()> {
        let session_key = self
            .session_key
            .as_ref()
            .ok_or(IntegrationError::NotAuthenticated)?;

        let url = format!("{}/track.updateNowPlaying", LASTFM_PROXY_URL);

        let mut body = json!({
            "sk": session_key,
            "artist": artist,
            "track": track,
        });

        if let Some(album_name) = album {
            body["album"] = json!(album_name);
        }

        let response = self.client.post(&url).json(&body).send().await?;

        if response.status().is_success() {
            log::debug!("Updated now playing: {} - {}", artist, track);
            Ok(())
        } else {
            let text = response.text().await.unwrap_or_default();
            Err(IntegrationError::internal(format!(
                "Update now playing failed: {}",
                text
            )))
        }
    }

    /// Get similar artists for a given artist name
    ///
    /// Uses Last.fm's artist.getSimilar which returns genre-accurate similarity.
    /// Requires authentication (user must have Last.fm connected).
    pub async fn get_similar_artists(
        &self,
        artist: &str,
        limit: u32,
    ) -> IntegrationResult<Vec<LastFmSimilarArtist>> {
        // artist.getSimilar is a public read endpoint - no session key needed.
        // The proxy handles the API key.
        let url = format!("{}/artist.getSimilar", LASTFM_PROXY_URL);

        let response = self
            .client
            .post(&url)
            .json(&json!({
                "artist": artist,
                "limit": limit,
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "Last.fm artist.getSimilar failed: {}",
                text
            )));
        }

        let text = response.text().await?;

        let data: serde_json::Value = serde_json::from_str(&text)?;

        // Handle Last.fm error responses
        if let Some(error) = data.get("error") {
            let message = data
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(IntegrationError::api(
                error.as_u64().unwrap_or(0) as u32,
                message.to_string(),
            ));
        }

        let artists = data
            .get("similarartists")
            .and_then(|sa| sa.get("artist"))
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let name = item.get("name")?.as_str()?.to_string();
                        // Last.fm returns match as string "0" to "1"
                        let match_score: f64 = item
                            .get("match")
                            .and_then(|m| {
                                m.as_str()
                                    .and_then(|s| s.parse().ok())
                                    .or_else(|| m.as_f64())
                            })
                            .unwrap_or(0.0);
                        let mbid = item
                            .get("mbid")
                            .and_then(|m| m.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string());

                        Some(LastFmSimilarArtist {
                            name,
                            match_score,
                            mbid,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(artists)
    }

    /// user.getTopArtists — top artists for the user (taste seed + known-artist set).
    ///
    /// `period` must be one of: `overall|7day|1month|3month|6month|12month`.
    /// Public read endpoint — no session key needed (the proxy injects the API key).
    pub async fn get_top_artists(
        &self,
        user: &str,
        period: &str,
        limit: u32,
    ) -> IntegrationResult<Vec<LastFmArtist>> {
        let url = format!("{}/user.getTopArtists", LASTFM_PROXY_URL);

        let response = self
            .client
            .post(&url)
            .json(&json!({
                "user": user,
                "period": period,
                "limit": limit,
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "Last.fm user.getTopArtists failed: {}",
                text
            )));
        }

        let text = response.text().await?;

        let data: serde_json::Value = serde_json::from_str(&text)?;

        // Handle Last.fm error responses
        if let Some(error) = data.get("error") {
            let message = data
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(IntegrationError::api(
                error.as_u64().unwrap_or(0) as u32,
                message.to_string(),
            ));
        }

        let artists = data
            .get("topartists")
            .and_then(|ta| ta.get("artist"))
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let name = item.get("name")?.as_str()?.to_string();
                        let mbid = extract_mbid(item);
                        let playcount = parse_u64(item.get("playcount"));
                        let image = extract_image(item);

                        Some(LastFmArtist {
                            name,
                            mbid,
                            playcount,
                            image,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(artists)
    }

    /// user.getTopTracks — top tracks for the user (period gives coarse recency).
    ///
    /// `period` must be one of: `overall|7day|1month|3month|6month|12month`.
    pub async fn get_top_tracks(
        &self,
        user: &str,
        period: &str,
        limit: u32,
    ) -> IntegrationResult<Vec<LastFmTrack>> {
        let url = format!("{}/user.getTopTracks", LASTFM_PROXY_URL);

        let response = self
            .client
            .post(&url)
            .json(&json!({
                "user": user,
                "period": period,
                "limit": limit,
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "Last.fm user.getTopTracks failed: {}",
                text
            )));
        }

        let text = response.text().await?;

        let data: serde_json::Value = serde_json::from_str(&text)?;

        if let Some(error) = data.get("error") {
            let message = data
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(IntegrationError::api(
                error.as_u64().unwrap_or(0) as u32,
                message.to_string(),
            ));
        }

        let tracks = data
            .get("toptracks")
            .and_then(|tt| tt.get("track"))
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let name = item.get("name")?.as_str()?.to_string();
                        let mbid = extract_mbid(item);
                        let artist_obj = item.get("artist");
                        let artist = artist_obj
                            .and_then(|a| a.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let artist_mbid = artist_obj.and_then(|a| extract_mbid(a));
                        let image = extract_image(item);

                        Some(LastFmTrack {
                            name,
                            artist,
                            artist_mbid,
                            mbid,
                            album: None,
                            image,
                            uts: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(tracks)
    }

    /// user.getLovedTracks — explicitly loved tracks (strong taste seed).
    pub async fn get_loved_tracks(
        &self,
        user: &str,
        limit: u32,
    ) -> IntegrationResult<Vec<LastFmTrack>> {
        let url = format!("{}/user.getLovedTracks", LASTFM_PROXY_URL);

        let response = self
            .client
            .post(&url)
            .json(&json!({
                "user": user,
                "limit": limit,
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "Last.fm user.getLovedTracks failed: {}",
                text
            )));
        }

        let text = response.text().await?;

        let data: serde_json::Value = serde_json::from_str(&text)?;

        if let Some(error) = data.get("error") {
            let message = data
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(IntegrationError::api(
                error.as_u64().unwrap_or(0) as u32,
                message.to_string(),
            ));
        }

        let tracks = data
            .get("lovedtracks")
            .and_then(|lt| lt.get("track"))
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let name = item.get("name")?.as_str()?.to_string();
                        let mbid = extract_mbid(item);
                        let artist_obj = item.get("artist");
                        let artist = artist_obj
                            .and_then(|a| a.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let artist_mbid = artist_obj.and_then(|a| extract_mbid(a));
                        let uts = extract_uts(item);
                        let image = extract_image(item);

                        Some(LastFmTrack {
                            name,
                            artist,
                            artist_mbid,
                            mbid,
                            album: None,
                            image,
                            uts,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(tracks)
    }

    /// user.getRecentTracks — timestamped scrobble history (max limit 200; paginate via `page`).
    pub async fn get_recent_tracks(
        &self,
        user: &str,
        limit: u32,
        page: u32,
    ) -> IntegrationResult<Vec<LastFmTrack>> {
        // Last.fm caps this endpoint at 200 items per page.
        let limit = limit.min(200);
        let url = format!("{}/user.getRecentTracks", LASTFM_PROXY_URL);

        let response = self
            .client
            .post(&url)
            .json(&json!({
                "user": user,
                "limit": limit,
                "page": page,
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "Last.fm user.getRecentTracks failed: {}",
                text
            )));
        }

        let text = response.text().await?;

        let data: serde_json::Value = serde_json::from_str(&text)?;

        if let Some(error) = data.get("error") {
            let message = data
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(IntegrationError::api(
                error.as_u64().unwrap_or(0) as u32,
                message.to_string(),
            ));
        }

        let tracks = data
            .get("recenttracks")
            .and_then(|rt| rt.get("track"))
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        // Skip the currently-playing track: it has no scrobble timestamp.
                        let now_playing = item
                            .get("@attr")
                            .and_then(|a| a.get("nowplaying"))
                            .and_then(|np| np.as_str())
                            == Some("true");
                        if now_playing {
                            return None;
                        }

                        let name = item.get("name")?.as_str()?.to_string();
                        let mbid = extract_mbid(item);

                        // `artist` may be an object ({"#text"|"name", "mbid"}) or a bare string.
                        let artist_obj = item.get("artist");
                        let artist = artist_obj
                            .and_then(|a| {
                                a.get("#text")
                                    .and_then(|v| v.as_str())
                                    .or_else(|| a.get("name").and_then(|v| v.as_str()))
                                    .or_else(|| a.as_str())
                            })
                            .unwrap_or_default()
                            .to_string();
                        let artist_mbid = artist_obj.and_then(|a| extract_mbid(a));

                        let album = item
                            .get("album")
                            .and_then(|al| al.get("#text"))
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string());

                        let uts = extract_uts(item);
                        let image = extract_image(item);

                        Some(LastFmTrack {
                            name,
                            artist,
                            artist_mbid,
                            mbid,
                            album,
                            image,
                            uts,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(tracks)
    }

    /// track.getSimilar — tracks similar to a seed track (raw match weight, NOT 0..1).
    pub async fn get_similar_tracks(
        &self,
        artist: &str,
        track: &str,
        limit: u32,
    ) -> IntegrationResult<Vec<LastFmSimilarTrack>> {
        let url = format!("{}/track.getSimilar", LASTFM_PROXY_URL);

        let response = self
            .client
            .post(&url)
            .json(&json!({
                "artist": artist,
                "track": track,
                "limit": limit,
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "Last.fm track.getSimilar failed: {}",
                text
            )));
        }

        let text = response.text().await?;

        let data: serde_json::Value = serde_json::from_str(&text)?;

        if let Some(error) = data.get("error") {
            let message = data
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(IntegrationError::api(
                error.as_u64().unwrap_or(0) as u32,
                message.to_string(),
            ));
        }

        let tracks = data
            .get("similartracks")
            .and_then(|st| st.get("track"))
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let name = item.get("name")?.as_str()?.to_string();
                        // Last.fm returns match as a string weight (e.g. "0.83492").
                        let match_score: f64 = item
                            .get("match")
                            .and_then(|m| {
                                m.as_str()
                                    .and_then(|s| s.parse().ok())
                                    .or_else(|| m.as_f64())
                            })
                            .unwrap_or(0.0);
                        let mbid = extract_mbid(item);
                        let artist = item
                            .get("artist")
                            .and_then(|a| a.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string();

                        Some(LastFmSimilarTrack {
                            name,
                            artist,
                            mbid,
                            match_score,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(tracks)
    }

    /// artist.getTopAlbums — an artist's most-popular albums (global playcount).
    ///
    /// Source for "Recommended Albums" candidates. Public read endpoint — no
    /// session key needed (the proxy injects the API key).
    pub async fn get_artist_top_albums(
        &self,
        artist: &str,
        limit: u32,
    ) -> IntegrationResult<Vec<LastFmAlbum>> {
        let url = format!("{}/artist.getTopAlbums", LASTFM_PROXY_URL);

        let response = self
            .client
            .post(&url)
            .json(&json!({
                "artist": artist,
                "limit": limit,
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "Last.fm artist.getTopAlbums failed: {}",
                text
            )));
        }

        let text = response.text().await?;

        let data: serde_json::Value = serde_json::from_str(&text)?;

        if let Some(error) = data.get("error") {
            let message = data
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(IntegrationError::api(
                error.as_u64().unwrap_or(0) as u32,
                message.to_string(),
            ));
        }

        let albums = data
            .get("topalbums")
            .and_then(|ta| ta.get("album"))
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let name = item.get("name")?.as_str()?.to_string();
                        let mbid = extract_mbid(item);
                        let artist_obj = item.get("artist");
                        let artist = artist_obj
                            .and_then(|a| a.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let artist_mbid = artist_obj.and_then(|a| extract_mbid(a));
                        let image = extract_image(item);
                        let playcount = parse_u64(item.get("playcount"));

                        Some(LastFmAlbum {
                            name,
                            artist,
                            artist_mbid,
                            mbid,
                            image,
                            playcount,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(albums)
    }

    /// Public `artist.getInfo` lookup used by Local Library's conservative
    /// portrait enrichment. No Last.fm user session is required; the proxy
    /// supplies the application key. `autocorrect` is returned to the caller
    /// as the canonical `name`, but consumers must still validate identity
    /// before attaching the image to a local tag spelling.
    pub async fn get_artist_info(&self, artist: &str) -> IntegrationResult<LastFmArtist> {
        let url = format!("{}/artist.getInfo", LASTFM_PROXY_URL);
        let response = self
            .client
            .post(&url)
            .json(&json!({
                "artist": artist,
                "autocorrect": 1,
            }))
            .send()
            .await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "Last.fm artist.getInfo failed: {text}"
            )));
        }
        let data: serde_json::Value = response.json().await?;
        if let Some(error) = data.get("error") {
            return Err(IntegrationError::api(
                error.as_u64().unwrap_or(0) as u32,
                data.get("message")
                    .and_then(|message| message.as_str())
                    .unwrap_or("Unknown error")
                    .to_string(),
            ));
        }
        let value = data.get("artist").ok_or_else(|| {
            IntegrationError::internal("Last.fm returned no artist object".to_string())
        })?;
        parse_artist_info(value, artist)
    }

    /// Public `album.getInfo` lookup used by the local metadata editor for a
    /// conservative artwork candidate. No Last.fm user session is required;
    /// the existing proxy supplies the application key.
    pub async fn get_album_info(
        &self,
        artist: &str,
        album: &str,
    ) -> IntegrationResult<LastFmAlbum> {
        let url = format!("{}/album.getInfo", LASTFM_PROXY_URL);
        let response = self
            .client
            .post(&url)
            .json(&json!({
                "artist": artist,
                "album": album,
                "autocorrect": 1,
            }))
            .send()
            .await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "Last.fm album.getInfo failed: {text}"
            )));
        }
        let data: serde_json::Value = response.json().await?;
        if let Some(error) = data.get("error") {
            return Err(IntegrationError::api(
                error.as_u64().unwrap_or(0) as u32,
                data.get("message")
                    .and_then(|message| message.as_str())
                    .unwrap_or("Unknown error")
                    .to_string(),
            ));
        }
        let value = data.get("album").ok_or_else(|| {
            IntegrationError::internal("Last.fm returned no album object".to_string())
        })?;
        let name = value
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or(album)
            .to_string();
        let artist = value
            .get("artist")
            .and_then(|artist| artist.as_str())
            .unwrap_or(artist)
            .to_string();
        Ok(LastFmAlbum {
            name,
            artist,
            artist_mbid: None,
            mbid: extract_mbid(value),
            image: extract_image(value),
            playcount: parse_u64(value.get("playcount")),
        })
    }

    /// user.getTopAlbums — the USER's scrobbled albums (their playcount).
    ///
    /// The "already heard" exclusion set for Recommended Albums. Public read
    /// endpoint — no session key needed (the proxy injects the API key).
    /// `period` must be one of: `overall|7day|1month|3month|6month|12month`.
    pub async fn get_user_top_albums(
        &self,
        user: &str,
        period: &str,
        limit: u32,
        page: u32,
    ) -> IntegrationResult<Vec<LastFmAlbum>> {
        let url = format!("{}/user.getTopAlbums", LASTFM_PROXY_URL);

        let response = self
            .client
            .post(&url)
            .json(&json!({
                "user": user,
                "period": period,
                "limit": limit,
                "page": page,
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "Last.fm user.getTopAlbums failed: {}",
                text
            )));
        }

        let text = response.text().await?;

        let data: serde_json::Value = serde_json::from_str(&text)?;

        if let Some(error) = data.get("error") {
            let message = data
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(IntegrationError::api(
                error.as_u64().unwrap_or(0) as u32,
                message.to_string(),
            ));
        }

        let albums = data
            .get("topalbums")
            .and_then(|ta| ta.get("album"))
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let name = item.get("name")?.as_str()?.to_string();
                        let mbid = extract_mbid(item);
                        let artist_obj = item.get("artist");
                        let artist = artist_obj
                            .and_then(|a| a.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let artist_mbid = artist_obj.and_then(|a| extract_mbid(a));
                        let image = extract_image(item);
                        let playcount = parse_u64(item.get("playcount"));

                        Some(LastFmAlbum {
                            name,
                            artist,
                            artist_mbid,
                            mbid,
                            image,
                            playcount,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(albums)
    }
}

/// Extract the largest image URL (last array entry's `#text`) from a Last.fm object.
/// Returns `None` when the array is missing, empty, or the URL is blank.
fn extract_image(value: &serde_json::Value) -> Option<String> {
    value
        .get("image")
        .and_then(|i| i.as_array())
        .and_then(|arr| arr.last())
        .and_then(|last| last.get("#text"))
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Extract a non-empty `mbid` field as `Option<String>` (empty strings become `None`).
fn extract_mbid(value: &serde_json::Value) -> Option<String> {
    value
        .get("mbid")
        .and_then(|m| m.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Extract a Unix timestamp from a Last.fm `date.uts` field (string or number).
fn extract_uts(value: &serde_json::Value) -> Option<i64> {
    value.get("date").and_then(|d| d.get("uts")).and_then(|u| {
        u.as_str()
            .and_then(|s| s.parse::<i64>().ok())
            .or_else(|| u.as_i64())
    })
}

/// Parse a `u64` that Last.fm may return as a JSON string or number; defaults to 0.
fn parse_u64(value: Option<&serde_json::Value>) -> u64 {
    value
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .or_else(|| v.as_u64())
        })
        .unwrap_or(0)
}

fn parse_artist_info(
    value: &serde_json::Value,
    fallback_name: &str,
) -> IntegrationResult<LastFmArtist> {
    Ok(LastFmArtist {
        name: value
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or(fallback_name)
            .to_string(),
        mbid: extract_mbid(value),
        playcount: parse_u64(value.get("stats").and_then(|stats| stats.get("playcount"))),
        image: extract_image(value),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artist_info_keeps_autocorrected_identity_and_largest_image() {
        let value = serde_json::json!({
            "name": "Beyoncé",
            "mbid": "859d0860-d480-4efd-970c-c05d5f1776b8",
            "stats": { "playcount": "42" },
            "image": [
                { "#text": "https://img/small.jpg", "size": "small" },
                { "#text": "https://img/large.jpg", "size": "extralarge" }
            ]
        });
        let parsed = parse_artist_info(&value, "Beyonce").unwrap();
        assert_eq!(parsed.name, "Beyoncé");
        assert_eq!(parsed.playcount, 42);
        assert_eq!(parsed.image.as_deref(), Some("https://img/large.jpg"));
        assert!(parsed.mbid.is_some());
    }
}
