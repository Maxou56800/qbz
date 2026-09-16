//! ListenBrainz API client
//!
//! Direct client for ListenBrainz submissions (no proxy needed - uses user token)

use reqwest::Client;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::models::*;
use crate::error::{IntegrationError, IntegrationResult};

/// ListenBrainz API base URL
const LISTENBRAINZ_API_URL: &str = "https://api.listenbrainz.org/1";

/// ListenBrainz client configuration
#[derive(Debug, Clone)]
pub struct ListenBrainzConfig {
    /// Whether ListenBrainz integration is enabled
    pub enabled: bool,
    /// User token from listenbrainz.org
    pub token: Option<String>,
    /// Username (set after token validation)
    pub user_name: Option<String>,
}

impl Default for ListenBrainzConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            token: None,
            user_name: None,
        }
    }
}

/// ListenBrainz API client
pub struct ListenBrainzClient {
    client: Client,
    config: Arc<Mutex<ListenBrainzConfig>>,
    version: String,
}

impl Default for ListenBrainzClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ListenBrainzClient {
    /// Create a new ListenBrainz client
    pub fn new() -> Self {
        Self::with_config(ListenBrainzConfig::default())
    }

    /// Create client with specific configuration
    pub fn with_config(config: ListenBrainzConfig) -> Self {
        let version = "1.0.0".to_string();
        let user_agent = format!(
            "QBZ/{} (https://github.com/vicrodh/qbz; qbz@vicrodh.dev)",
            version
        );

        let builder =
            qbz_net_proxy::apply_current(Client::builder()).unwrap_or_else(|_| Client::builder());
        let client = builder
            .user_agent(&user_agent)
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            client,
            config: Arc::new(Mutex::new(config)),
            version,
        }
    }

    /// Set the application version for submission metadata
    pub fn set_version(&mut self, version: impl Into<String>) {
        self.version = version.into();
    }

    /// Check if ListenBrainz integration is enabled
    pub async fn is_enabled(&self) -> bool {
        self.config.lock().await.enabled
    }

    /// Enable or disable ListenBrainz integration
    pub async fn set_enabled(&self, enabled: bool) {
        self.config.lock().await.enabled = enabled;
    }

    /// Check if authenticated (has valid token)
    pub async fn is_authenticated(&self) -> bool {
        let config = self.config.lock().await;
        config.token.is_some() && config.user_name.is_some()
    }

    /// Get current status
    pub async fn get_status(&self) -> ListenBrainzStatus {
        let config = self.config.lock().await;
        ListenBrainzStatus {
            connected: config.token.is_some() && config.user_name.is_some(),
            user_name: config.user_name.clone(),
            enabled: config.enabled,
        }
    }

    /// Set user token and validate it
    pub async fn set_token(&self, token: &str) -> IntegrationResult<UserInfo> {
        // Validate token first
        let validation = self.validate_token(token).await?;

        if !validation.valid {
            return Err(IntegrationError::AuthFailed(validation.message));
        }

        let user_name = validation.user_name.ok_or_else(|| {
            IntegrationError::AuthFailed("Token valid but no username returned".into())
        })?;

        // Store validated token and username
        {
            let mut config = self.config.lock().await;
            config.token = Some(token.to_string());
            config.user_name = Some(user_name.clone());
        }

        log::info!("ListenBrainz connected");

        Ok(UserInfo { user_name })
    }

    /// Restore token from saved session (without re-validating)
    pub async fn restore_token(&self, token: String, user_name: String) {
        let mut config = self.config.lock().await;
        config.token = Some(token);
        config.user_name = Some(user_name);
    }

    /// Get current token (for persistence)
    pub async fn get_token(&self) -> Option<String> {
        self.config.lock().await.token.clone()
    }

    /// Get current username
    pub async fn get_user_name(&self) -> Option<String> {
        self.config.lock().await.user_name.clone()
    }

    /// Disconnect (clear token)
    pub async fn disconnect(&self) {
        let mut config = self.config.lock().await;
        config.token = None;
        config.user_name = None;
        log::info!("ListenBrainz disconnected");
    }

    /// Validate a token with ListenBrainz API
    async fn validate_token(&self, token: &str) -> IntegrationResult<TokenValidationResponse> {
        let url = format!("{}/validate-token", LISTENBRAINZ_API_URL);

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Token {}", token))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::AuthFailed(format!(
                "Token validation failed: {} - {}",
                status, text
            )));
        }

        response
            .json::<TokenValidationResponse>()
            .await
            .map_err(Into::into)
    }

    /// Submit "now playing" notification
    pub async fn submit_playing_now(
        &self,
        artist: &str,
        track: &str,
        album: Option<&str>,
        additional_info: Option<AdditionalInfo>,
    ) -> IntegrationResult<()> {
        let token = {
            let config = self.config.lock().await;
            if !config.enabled {
                return Ok(()); // Silently skip if disabled
            }
            config.token.clone()
        };

        let token = token.ok_or(IntegrationError::NotAuthenticated)?;

        let info = self.prepare_additional_info(additional_info);

        let payload = SubmitListensPayload {
            listen_type: ListenType::PlayingNow,
            payload: vec![Listen {
                listened_at: None, // Not used for playing_now
                track_metadata: TrackMetadata {
                    artist_name: artist.to_string(),
                    track_name: track.to_string(),
                    release_name: album.map(|s| s.to_string()),
                    additional_info: Some(info),
                },
            }],
        };

        self.submit_listens(&token, &payload).await
    }

    /// Submit a scrobble (track finished playing)
    pub async fn submit_listen(
        &self,
        artist: &str,
        track: &str,
        album: Option<&str>,
        timestamp: i64,
        additional_info: Option<AdditionalInfo>,
    ) -> IntegrationResult<()> {
        let token = {
            let config = self.config.lock().await;
            if !config.enabled {
                return Ok(()); // Silently skip if disabled
            }
            config.token.clone()
        };

        let token = token.ok_or(IntegrationError::NotAuthenticated)?;

        let info = self.prepare_additional_info(additional_info);

        let payload = SubmitListensPayload {
            listen_type: ListenType::Single,
            payload: vec![Listen {
                listened_at: Some(timestamp),
                track_metadata: TrackMetadata {
                    artist_name: artist.to_string(),
                    track_name: track.to_string(),
                    release_name: album.map(|s| s.to_string()),
                    additional_info: Some(info),
                },
            }],
        };

        self.submit_listens(&token, &payload).await
    }

    /// Prepare additional info with QBZ identifiers
    fn prepare_additional_info(&self, info: Option<AdditionalInfo>) -> AdditionalInfo {
        let mut info = info.unwrap_or_default();
        info.media_player = "QBZ".to_string();
        info.media_player_version = self.version.clone();
        info.submission_client = "QBZ".to_string();
        info.submission_client_version = self.version.clone();
        info
    }

    /// Internal: Submit listens to API
    async fn submit_listens(
        &self,
        token: &str,
        payload: &SubmitListensPayload,
    ) -> IntegrationResult<()> {
        let url = format!("{}/submit-listens", LISTENBRAINZ_API_URL);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Token {}", token))
            .header("Content-Type", "application/json")
            .json(payload)
            .send()
            .await?;

        if response.status().is_success() {
            let listen_type = match payload.listen_type {
                ListenType::PlayingNow => "now playing",
                ListenType::Single => "scrobble",
            };
            if let Some(listen) = payload.payload.first() {
                log::debug!(
                    "ListenBrainz {}: {} - {}",
                    listen_type,
                    listen.track_metadata.artist_name,
                    listen.track_metadata.track_name
                );
            }
            Ok(())
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(IntegrationError::internal(format!(
                "ListenBrainz submission failed: {} - {}",
                status, text
            )))
        }
    }

    /// Collaborative-filtering recommendations (PRIMARY personalized recommender).
    ///
    /// `GET /cf/recommendation/user/{user_name}/recording?count={count}`
    ///
    /// The `Authorization` header is sent only when a token is configured (raises
    /// rate limits but is not required — this is a public read keyed by username).
    /// HTTP 204/404 and empty bodies are treated as "no data" -> `Ok(vec![])`.
    /// Parses `payload.mbids[]` into `{recording_mbid, score, latest_listened_at}`.
    /// `latest_listened_at` is an ISO-8601 string OR null (null/absent = never listened).
    pub async fn get_cf_recommendations(
        &self,
        user_name: &str,
        count: u32,
    ) -> IntegrationResult<Vec<CfRecommendation>> {
        let token = self.config.lock().await.token.clone();

        let url = format!(
            "{}/cf/recommendation/user/{}/recording",
            LISTENBRAINZ_API_URL, user_name
        );

        let mut request = self
            .client
            .get(&url)
            .query(&[("count", count.to_string())]);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Token {}", token));
        }

        let response = request.send().await?;
        let status = response.status();

        // 204 No Content / 404 Not Found -> the user simply has no recommendations.
        if status == reqwest::StatusCode::NO_CONTENT || status == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "ListenBrainz CF recommendations failed: {} - {}",
                status, text
            )));
        }

        let body = response.text().await.unwrap_or_default();
        if body.trim().is_empty() {
            return Ok(vec![]);
        }
        // Defensive parse: tolerate any malformed payload as "no data".
        let json: serde_json::Value = match serde_json::from_str(&body) {
            Ok(value) => value,
            Err(_) => return Ok(vec![]),
        };

        let mbids = json
            .get("payload")
            .and_then(|payload| payload.get("mbids"))
            .and_then(|mbids| mbids.as_array())
            .cloned()
            .unwrap_or_default();

        let mut recommendations = Vec::with_capacity(mbids.len());
        for item in mbids {
            let recording_mbid = match item
                .get("recording_mbid")
                .and_then(|value| value.as_str())
            {
                Some(mbid) if !mbid.is_empty() => mbid.to_string(),
                // An entry with no recording_mbid is useless downstream; skip it.
                _ => continue,
            };
            let score = item.get("score").and_then(|value| value.as_f64()).unwrap_or(0.0);
            let latest_listened_at = item
                .get("latest_listened_at")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());

            recommendations.push(CfRecommendation {
                recording_mbid,
                score,
                latest_listened_at,
            });
        }

        Ok(recommendations)
    }

    /// Raw scrobble history with timestamps.
    ///
    /// `GET /user/{user_name}/listens?count={count}`
    ///
    /// The `Authorization` header is sent only when a token is configured.
    /// HTTP 204/404 and empty bodies are treated as "no data" -> `Ok(vec![])`.
    /// Parses `payload.listens[]` into
    /// `{listened_at, track_metadata.artist_name, track_metadata.track_name,
    ///   track_metadata.mbid_mapping.recording_mbid}`.
    pub async fn get_recent_listens(
        &self,
        user_name: &str,
        count: u32,
    ) -> IntegrationResult<Vec<LbListen>> {
        let token = self.config.lock().await.token.clone();

        let url = format!("{}/user/{}/listens", LISTENBRAINZ_API_URL, user_name);

        let mut request = self
            .client
            .get(&url)
            .query(&[("count", count.to_string())]);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Token {}", token));
        }

        let response = request.send().await?;
        let status = response.status();

        if status == reqwest::StatusCode::NO_CONTENT || status == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "ListenBrainz recent listens failed: {} - {}",
                status, text
            )));
        }

        let body = response.text().await.unwrap_or_default();
        if body.trim().is_empty() {
            return Ok(vec![]);
        }
        let json: serde_json::Value = match serde_json::from_str(&body) {
            Ok(value) => value,
            Err(_) => return Ok(vec![]),
        };

        let listens = json
            .get("payload")
            .and_then(|payload| payload.get("listens"))
            .and_then(|listens| listens.as_array())
            .cloned()
            .unwrap_or_default();

        let mut parsed = Vec::with_capacity(listens.len());
        for item in listens {
            let listened_at = item
                .get("listened_at")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            let track_metadata = item.get("track_metadata");
            let artist_name = track_metadata
                .and_then(|meta| meta.get("artist_name"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let track_name = track_metadata
                .and_then(|meta| meta.get("track_name"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let recording_mbid = track_metadata
                .and_then(|meta| meta.get("mbid_mapping"))
                .and_then(|mapping| mapping.get("recording_mbid"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());

            parsed.push(LbListen {
                listened_at,
                artist_name,
                track_name,
                recording_mbid,
            });
        }

        Ok(parsed)
    }

    /// Hydrate CF `recording_mbid`s into names + artist mbids + cover art.
    ///
    /// `GET /metadata/recording/?recording_mbids={comma-joined}&inc=artist+release`
    ///
    /// The response is a JSON OBJECT keyed by `recording_mbid`:
    /// ```text
    /// { "<mbid>": { "recording": {"name": ...},
    ///               "artist": {"name": ..., "artists": [{"artist_mbid": ...}, ...]},
    ///               "release": {"name": ..., "caa_id": ..., "caa_release_mbid": ...} } }
    /// ```
    /// Iterates the object entries; the KEY is the `recording_mbid`. Empty input
    /// short-circuits to `Ok(vec![])`. HTTP 204/404 and empty bodies are also
    /// treated as "no data".
    pub async fn get_metadata_recordings(
        &self,
        recording_mbids: &[String],
    ) -> IntegrationResult<Vec<LbRecordingMeta>> {
        if recording_mbids.is_empty() {
            return Ok(vec![]);
        }

        let token = self.config.lock().await.token.clone();

        let url = format!("{}/metadata/recording/", LISTENBRAINZ_API_URL);
        let joined = recording_mbids.join(",");

        // `inc=artist release` is form-encoded to `inc=artist+release` by reqwest,
        // which is exactly the value ListenBrainz expects.
        let mut request = self.client.get(&url).query(&[
            ("recording_mbids", joined.as_str()),
            ("inc", "artist release"),
        ]);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Token {}", token));
        }

        let response = request.send().await?;
        let status = response.status();

        if status == reqwest::StatusCode::NO_CONTENT || status == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "ListenBrainz metadata recordings failed: {} - {}",
                status, text
            )));
        }

        let body = response.text().await.unwrap_or_default();
        if body.trim().is_empty() {
            return Ok(vec![]);
        }
        let json: serde_json::Value = match serde_json::from_str(&body) {
            Ok(value) => value,
            Err(_) => return Ok(vec![]),
        };

        let object = match json.as_object() {
            Some(object) => object,
            None => return Ok(vec![]),
        };

        let mut recordings = Vec::with_capacity(object.len());
        for (recording_mbid, entry) in object {
            let recording_name = entry
                .get("recording")
                .and_then(|recording| recording.get("name"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();

            let artist = entry.get("artist");
            let artist_name = artist
                .and_then(|artist| artist.get("name"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let artist_mbids = artist
                .and_then(|artist| artist.get("artists"))
                .and_then(|artists| artists.as_array())
                .map(|artists| {
                    artists
                        .iter()
                        .filter_map(|credit| {
                            credit
                                .get("artist_mbid")
                                .and_then(|value| value.as_str())
                                .map(|value| value.to_string())
                        })
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();

            let release = entry.get("release");
            let release_name = release
                .and_then(|release| release.get("name"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let caa_id = release
                .and_then(|release| release.get("caa_id"))
                .and_then(|value| value.as_i64());
            let caa_release_mbid = release
                .and_then(|release| release.get("caa_release_mbid"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());

            recordings.push(LbRecordingMeta {
                recording_mbid: recording_mbid.clone(),
                recording_name,
                artist_name,
                artist_mbids,
                release_name,
                caa_id,
                caa_release_mbid,
            });
        }

        Ok(recordings)
    }

    /// List the "Created for you" curated playlists (Weekly Jams = familiar,
    /// Weekly Exploration = discovery, Top Discoveries, etc.).
    ///
    /// `GET /user/{user_name}/playlists/createdfor?count={count}`
    ///
    /// Public read; the `Authorization` header is sent only when a token is
    /// configured. HTTP 204/404 and empty/malformed bodies are treated as
    /// "no data" -> `Ok(vec![])`.
    ///
    /// Response shape: `{ playlists: [ { playlist: {...} }, ... ] }`. For each
    /// inner `playlist` object:
    /// - `title`
    /// - `date` -> `created_at`
    /// - `annotation`
    /// - `playlist_mbid` = LAST path segment of `identifier` (string OR array;
    ///   e.g. `"https://listenbrainz.org/playlist/{mbid}"`)
    /// - `source_patch` = `extension["https://musicbrainz.org/doc/jspf#playlist"]`
    ///   `.additional_metadata.algorithm_metadata.source_patch`
    pub async fn get_created_for_playlists(
        &self,
        user_name: &str,
        count: u32,
    ) -> IntegrationResult<Vec<LbPlaylistMeta>> {
        let token = self.config.lock().await.token.clone();

        let url = format!(
            "{}/user/{}/playlists/createdfor",
            LISTENBRAINZ_API_URL, user_name
        );

        let mut request = self
            .client
            .get(&url)
            .query(&[("count", count.to_string())]);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Token {}", token));
        }

        let response = request.send().await?;
        let status = response.status();

        if status == reqwest::StatusCode::NO_CONTENT || status == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "ListenBrainz created-for playlists failed: {} - {}",
                status, text
            )));
        }

        let body = response.text().await.unwrap_or_default();
        if body.trim().is_empty() {
            return Ok(vec![]);
        }
        let json: serde_json::Value = match serde_json::from_str(&body) {
            Ok(value) => value,
            Err(_) => return Ok(vec![]),
        };

        let playlists = json
            .get("playlists")
            .and_then(|playlists| playlists.as_array())
            .cloned()
            .unwrap_or_default();

        let mut parsed = Vec::with_capacity(playlists.len());
        for wrapper in playlists {
            // Each array entry wraps the real object under a `playlist` key.
            let playlist = match wrapper.get("playlist") {
                Some(playlist) => playlist,
                None => continue,
            };

            let playlist_mbid = match playlist
                .get("identifier")
                .and_then(last_identifier_segment)
            {
                Some(mbid) => mbid,
                // No usable playlist id -> useless downstream; skip it.
                None => continue,
            };

            let title = playlist
                .get("title")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let created_at = playlist
                .get("date")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let annotation = playlist
                .get("annotation")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let source_patch = playlist
                .get("extension")
                .and_then(|ext| ext.get("https://musicbrainz.org/doc/jspf#playlist"))
                .and_then(|ext| ext.get("additional_metadata"))
                .and_then(|meta| meta.get("algorithm_metadata"))
                .and_then(|algo| algo.get("source_patch"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());

            parsed.push(LbPlaylistMeta {
                playlist_mbid,
                title,
                source_patch,
                annotation,
                created_at,
            });
        }

        Ok(parsed)
    }

    /// Fetch one playlist's tracks (JSPF).
    ///
    /// `GET /playlist/{playlist_mbid}`
    ///
    /// Public read; the `Authorization` header is sent only when a token is
    /// configured. HTTP 204/404 and empty/malformed bodies are treated as
    /// "no data" -> `Ok(vec![])`.
    ///
    /// Response shape: `{ playlist: { track: [ ... ] } }`. For each track:
    /// - `title`
    /// - `creator` -> `artist_name`
    /// - `album` -> `release_name`
    /// - `recording_mbid` = LAST path segment of the track `identifier`
    ///   (string OR array)
    /// - `caa_id` + `caa_release_mbid` from
    ///   `extension["https://musicbrainz.org/doc/jspf#track"].additional_metadata`
    pub async fn get_playlist_tracks(
        &self,
        playlist_mbid: &str,
    ) -> IntegrationResult<Vec<LbPlaylistTrack>> {
        let token = self.config.lock().await.token.clone();

        let url = format!("{}/playlist/{}", LISTENBRAINZ_API_URL, playlist_mbid);

        let mut request = self.client.get(&url);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Token {}", token));
        }

        let response = request.send().await?;
        let status = response.status();

        if status == reqwest::StatusCode::NO_CONTENT || status == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "ListenBrainz playlist tracks failed: {} - {}",
                status, text
            )));
        }

        let body = response.text().await.unwrap_or_default();
        if body.trim().is_empty() {
            return Ok(vec![]);
        }
        let json: serde_json::Value = match serde_json::from_str(&body) {
            Ok(value) => value,
            Err(_) => return Ok(vec![]),
        };

        let tracks = json
            .get("playlist")
            .and_then(|playlist| playlist.get("track"))
            .and_then(|track| track.as_array())
            .cloned()
            .unwrap_or_default();

        let mut parsed = Vec::with_capacity(tracks.len());
        for track in tracks {
            let title = track
                .get("title")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let artist_name = track
                .get("creator")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let release_name = track
                .get("album")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let recording_mbid = track.get("identifier").and_then(last_identifier_segment);

            let additional = track
                .get("extension")
                .and_then(|ext| ext.get("https://musicbrainz.org/doc/jspf#track"))
                .and_then(|ext| ext.get("additional_metadata"));
            let caa_id = additional
                .and_then(|meta| meta.get("caa_id"))
                .and_then(|value| value.as_i64());
            let caa_release_mbid = additional
                .and_then(|meta| meta.get("caa_release_mbid"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());

            parsed.push(LbPlaylistTrack {
                recording_mbid,
                title,
                artist_name,
                release_name,
                caa_id,
                caa_release_mbid,
            });
        }

        Ok(parsed)
    }

    /// Personalized fresh releases.
    ///
    /// `GET /user/{user_name}/fresh_releases?days={days}`
    ///
    /// `days` is clamped to `1..=90`. Public read; the `Authorization` header
    /// is sent only when a token is configured. HTTP 204/404 and empty/malformed
    /// bodies are treated as "no data" -> `Ok(vec![])`.
    ///
    /// Response shape: `{ payload: { releases: [ ... ] } }`. For each release:
    /// - `release_name`
    /// - `artist_credit_name`
    /// - `release_mbid`
    /// - `release_group_mbid`
    /// - `release_group_primary_type` -> `primary_type`
    /// - `caa_id`
    /// - `caa_release_mbid`
    /// - `release_date`
    /// - `listen_count`
    pub async fn get_fresh_releases(
        &self,
        user_name: &str,
        days: u32,
    ) -> IntegrationResult<Vec<LbFreshRelease>> {
        let token = self.config.lock().await.token.clone();

        let days = days.clamp(1, 90);

        let url = format!("{}/user/{}/fresh_releases", LISTENBRAINZ_API_URL, user_name);

        let mut request = self.client.get(&url).query(&[("days", days.to_string())]);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Token {}", token));
        }

        let response = request.send().await?;
        let status = response.status();

        if status == reqwest::StatusCode::NO_CONTENT || status == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(IntegrationError::internal(format!(
                "ListenBrainz fresh releases failed: {} - {}",
                status, text
            )));
        }

        let body = response.text().await.unwrap_or_default();
        if body.trim().is_empty() {
            return Ok(vec![]);
        }
        let json: serde_json::Value = match serde_json::from_str(&body) {
            Ok(value) => value,
            Err(_) => return Ok(vec![]),
        };

        let releases = json
            .get("payload")
            .and_then(|payload| payload.get("releases"))
            .and_then(|releases| releases.as_array())
            .cloned()
            .unwrap_or_default();

        let mut parsed = Vec::with_capacity(releases.len());
        for release in releases {
            let release_name = release
                .get("release_name")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let artist_credit_name = release
                .get("artist_credit_name")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let release_mbid = release
                .get("release_mbid")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let release_group_mbid = release
                .get("release_group_mbid")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let primary_type = release
                .get("release_group_primary_type")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let caa_id = release.get("caa_id").and_then(|value| value.as_i64());
            let caa_release_mbid = release
                .get("caa_release_mbid")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let release_date = release
                .get("release_date")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let listen_count = release.get("listen_count").and_then(|value| value.as_u64());

            parsed.push(LbFreshRelease {
                release_name,
                artist_credit_name,
                release_mbid,
                release_group_mbid,
                primary_type,
                caa_id,
                caa_release_mbid,
                release_date,
                listen_count,
            });
        }

        Ok(parsed)
    }
}

/// Extract the last `/`-delimited segment of a JSPF `identifier` value.
///
/// ListenBrainz returns the `identifier` either as a single string
/// (`"https://listenbrainz.org/playlist/{mbid}"`) or as an array of such
/// strings. Returns the last non-empty path segment of the first usable value,
/// or `None` when nothing parseable is present.
fn last_identifier_segment(identifier: &serde_json::Value) -> Option<String> {
    fn last_segment(raw: &str) -> Option<String> {
        raw.trim_end_matches('/')
            .rsplit('/')
            .next()
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .map(|segment| segment.to_string())
    }

    match identifier {
        serde_json::Value::String(raw) => last_segment(raw),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(serde_json::Value::as_str)
            .find_map(last_segment),
        _ => None,
    }
}
