//! Jellyfin integration — the protocol half.
//!
//! Sibling of `qbz-plex`, and deliberately shaped like it so `qbz-source`'s
//! `JellyfinSource` reads like `PlexSource`. Frontend-agnostic: no Qt, no
//! settings store, no cache schema — this crate speaks HTTP and returns DTOs.
//!
//! Everything below was MEASURED against Jellyfin 10.11.11 on 2026-08-20; the
//! numbers and the field shapes live in
//! `qbz-nix-docs/qt-frontend/2026-08-20-jellyfin-subsonic/01-research.md`.
//! The request shapes were re-verified against 12.1.0 on 2026-09-15 (#502).
//!
//! # Authorization: one header, every version
//!
//! Jellyfin 12.0 disabled "legacy authorization" by default, and its migration
//! turns it off on upgraded servers too (server PR #15559). With it off the
//! server no longer reads `X-Emby-Token`, `X-MediaBrowser-Token`,
//! `X-Emby-Authorization` or the `api_key` query parameter. Sign-in still
//! succeeds (it carries no token), and then EVERY following request answers
//! 401 — exactly the "Signed in, but library access failed" users reported.
//!
//! The only forms read with legacy authorization off are the `Authorization:
//! MediaBrowser …, Token="…"` header and the `ApiKey` query parameter. Both
//! are also read by every server back to at least 10.8 (checked in the
//! server's `AuthorizationContext`), so this crate uses nothing else.
//!
//! # The three facts that shape this file
//!
//! **1. `?static=true` is bit-perfect, and it was verified by md5.** The
//! response bytes of `/Audio/{id}/stream?static=true` are byte-identical to the
//! file on disk (checked against the server's own filesystem, not inferred from
//! a content type). It answers with `Content-Length` and `Accept-Ranges: bytes`,
//! which is exactly what QBZ's progressive feeder needs.
//!
//! The transcode path is unmistakable when you hit it: it answers
//! `Transfer-Encoding: chunked` with **no `Content-Length`**. That is not a
//! footnote — it is a cheap runtime assertion, and [`is_direct_response`]
//! exists so a regression is caught rather than quietly resampled. QBZ never
//! asks for a transcode and must never accept one.
//!
//! **2. Quality is available with the listing, but it is not free.** `BitDepth` and
//! `SampleRate` live in `MediaSources[].MediaStreams[]`, which requires
//! `Fields=MediaSources`. Measured on a 500-item page: 0.25 s without it, 4.56 s
//! with. Full sweep of 4924 tracks: **45.8 s**. `Fields=MediaStreams` trims 29 %
//! of the bytes and saves nothing — the cost is server-side media-info
//! hydration, not transfer. Catalog discovery therefore omits `MediaSources`;
//! a secondary, resumable hydration pass requests it only for bounded id
//! batches, with visible and queued rows allowed to jump ahead.
//!
//! **3. Artwork needs no credentials.** `/Items/{id}/Images/Primary` answers
//! 200 unauthenticated. Unlike Plex, whose thumb url has to be re-tokenized on
//! every pass, a Jellyfin cover url is STABLE — so its cache key and its fetch
//! url are the same string, and it can be memoized forever.
//!
//! # What is deliberately absent
//!
//! No writes. No favourites, no playlists, no playback reporting. Read + play
//! is the 2.1 scope; both are supported by the server and neither is exercised
//! by anything today, so shipping them untested would be inventing surface.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

/// Matches `qbz-plex`'s client budget: a hung request on a captive portal must
/// not pin a caller indefinitely. The STREAM is never fetched through this
/// client — the player's feeder owns that, with no deadline, because a hi-res
/// FLAC over a slow link legitimately outlives any probe budget.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Page size for the cheap essential-metadata sweep. It stays inside the
/// catalog's initial 100–250 row budget; the expensive quality pass has its
/// own, smaller batch size.
pub const PAGE_SIZE: u32 = 250;

/// Maximum ids in one secondary `MediaSources` request. A priority batch must
/// be small enough that newly visible/queued tracks are not stuck behind
/// several seconds of unrelated server-side probing.
pub const QUALITY_BATCH_SIZE: usize = 50;

/// Cover size this client requests, app-wide. One size is deliberate, for the
/// same reason `artwork_qt::PLEX_THUMB_PX` is: the cache key is the url, so a
/// second size means a second download of the same cover.
pub const IMAGE_PX: u32 = 256;

/// The larger tier, for hero / immersive slots only.
pub const IMAGE_PX_LARGE: u32 = 1024;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Anything that can go wrong talking to a Jellyfin server.
///
/// A plain `String` would have done what `qbz-plex` does, but the caller has a
/// real decision to make on `Unauthorized` (re-authenticate) that it cannot
/// make by matching on prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JellyfinError {
    /// The request never completed (DNS, refused, timed out).
    Transport(String),
    /// 401/403 — the token is gone or the user was disabled.
    Unauthorized,
    /// The password sign-in endpoint rejected the attempt.
    AuthenticationRejected,
    QuickConnectDisabled,
    QuickConnectExpired,
    /// Any other non-success status.
    Status(u16),
    /// The body was not the shape this client expects.
    Decode(String),
    /// The server answered, and the answer is "no such thing".
    NotFound(String),
}

impl std::fmt::Display for JellyfinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JellyfinError::Transport(e) => write!(f, "jellyfin request failed: {e}"),
            JellyfinError::Unauthorized => write!(
                f,
                "jellyfin rejected access to this resource (HTTP 401/403)"
            ),
            JellyfinError::AuthenticationRejected => {
                write!(f, "jellyfin rejected the sign-in (HTTP 401/403)")
            }
            JellyfinError::QuickConnectDisabled => {
                write!(f, "Quick Connect is disabled on this server.")
            }
            JellyfinError::QuickConnectExpired => {
                write!(f, "Quick Connect expired. Request a new code.")
            }
            JellyfinError::Status(s) => write!(f, "jellyfin answered {s}"),
            JellyfinError::Decode(e) => write!(f, "jellyfin response not understood: {e}"),
            JellyfinError::NotFound(what) => write!(f, "jellyfin has no {what}"),
        }
    }
}

impl std::error::Error for JellyfinError {}

type Result<T> = std::result::Result<T, JellyfinError>;

// ---------------------------------------------------------------------------
// DTOs — only the fields QBZ consumes
// ---------------------------------------------------------------------------

/// `GET /System/Info/Public` — the unauthenticated probe behind "test
/// connection". It also validates a URL before the user is asked for a
/// password, which is the difference between "wrong address" and "wrong
/// password" in the settings panel.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ServerInfo {
    pub server_name: String,
    pub version: String,
    pub id: String,
    #[serde(default)]
    pub startup_wizard_completed: bool,
}

/// What `POST /Users/AuthenticateByName` hands back.
#[derive(Debug, Clone)]
pub struct Session {
    pub access_token: String,
    pub user_id: String,
    pub user_name: String,
    pub server_id: String,
}

/// One music library ("view"), from `GET /Users/{uid}/Views`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MusicLibrary {
    pub id: String,
    pub name: String,
}

/// One audio item, flattened out of the `/Items` envelope into the shape the
/// cache stores.
///
/// Field names are QBZ's, not Jellyfin's: this is the boundary where the
/// vendor's vocabulary stops.
#[derive(Debug, Clone, PartialEq)]
pub struct JellyfinTrack {
    /// The item id — the ONLY identifier. Everything else is display data.
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    /// The parent `MusicAlbum` item id. Present on every row measured (0 of
    /// 4924 lacked it), and it is what groups a library into albums.
    pub album_id: String,
    /// `None` on 75 of 4924 measured rows — ordering must tolerate it.
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub duration_ms: u64,
    pub year: Option<u32>,
    /// Every genre Jellyfin supplied, in server order. `genre` remains the
    /// compatibility primary value for older cache readers.
    pub genres: Vec<String>,
    pub genre: Option<String>,
    /// Container as the server names it (`flac`, `mp3`, `ape`).
    pub container: String,
    pub codec: Option<String>,
    /// `None` for lossy codecs — MP3 reports no bit depth. Subsonic reports 0
    /// for the same case; both mean "not applicable" and the mapper folds them.
    pub bit_depth: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u32>,
    pub bitrate_bps: Option<u32>,
    /// The album's primary-image tag. Absent on 1053 of 4924 measured rows
    /// (21 %). Its absence does not mean the album has no image: Jellyfin's
    /// primary-image endpoint remains addressable by `album_id` without a tag.
    pub album_image_tag: Option<String>,
    /// MusicBrainz recording id from `ProviderIds.MusicBrainzTrack`, when the
    /// server has one. Cross-source identity for the listen log.
    pub recording_mbid: Option<String>,
    /// This audio item's own primary-image tag. Jellyfin/Navidrome can expose
    /// a different embedded or folder cover for each disc while still grouping
    /// every row under one `MusicAlbum`; this must outrank the collection art.
    pub item_image_tag: Option<String>,
    /// The server's own path. Real, unlike Subsonic's synthesised one, but QBZ
    /// never opens it: the file lives on the server. Kept for diagnostics only.
    pub server_path: Option<String>,
}

/// Quality fields hydrated independently from the essential track listing.
/// The item id is the join key; no list position is trusted because Jellyfin
/// may omit an item deleted between discovery and hydration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JellyfinTrackQuality {
    pub id: String,
    pub container: String,
    pub codec: Option<String>,
    pub bit_depth: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u32>,
    pub bitrate_bps: Option<u32>,
}

impl JellyfinTrack {
    /// Jellyfin counts in 100 ns ticks.
    fn ticks_to_ms(ticks: i64) -> u64 {
        (ticks.max(0) as u64) / 10_000
    }
}

// --- wire shapes (private) --------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ItemsEnvelope<T> {
    items: Vec<T>,
    total_record_count: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ViewDto {
    id: String,
    name: String,
    #[serde(default)]
    collection_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AuthDto {
    access_token: String,
    server_id: String,
    user: AuthUserDto,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AuthUserDto {
    id: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AudioDto {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    album: Option<String>,
    #[serde(default)]
    album_id: Option<String>,
    #[serde(default)]
    album_artist: Option<String>,
    #[serde(default)]
    artists: Vec<String>,
    #[serde(default)]
    index_number: Option<u32>,
    #[serde(default)]
    parent_index_number: Option<u32>,
    #[serde(default)]
    run_time_ticks: Option<i64>,
    #[serde(default)]
    production_year: Option<u32>,
    #[serde(default)]
    genres: Vec<String>,
    #[serde(default)]
    container: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    album_primary_image_tag: Option<String>,
    #[serde(default)]
    image_tags: HashMap<String, String>,
    #[serde(default)]
    media_sources: Vec<MediaSourceDto>,
    /// `ProviderIds` — Jellyfin's own name for external ids; for an audio
    /// item `MusicBrainzTrack` is the RECORDING id (its scanner writes the
    /// file's MUSICBRAINZ_TRACKID there). Requested via `Fields=ProviderIds`.
    #[serde(default)]
    provider_ids: HashMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MediaSourceDto {
    #[serde(default)]
    media_streams: Vec<MediaStreamDto>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MediaStreamDto {
    #[serde(default)]
    #[serde(rename = "Type")]
    kind: String,
    #[serde(default)]
    codec: Option<String>,
    #[serde(default)]
    bit_depth: Option<u32>,
    #[serde(default)]
    sample_rate: Option<u32>,
    #[serde(default)]
    channels: Option<u32>,
    #[serde(default)]
    bit_rate: Option<u32>,
}

impl AudioDto {
    fn audio_stream(&self) -> Option<&MediaStreamDto> {
        self.media_sources.first().and_then(|source| {
            source
                .media_streams
                .iter()
                .find(|stream| stream.kind == "Audio")
        })
    }

    fn into_quality(self) -> JellyfinTrackQuality {
        let (codec, bit_depth, sample_rate_hz, channels, bitrate_bps) = self
            .audio_stream()
            .map(|stream| {
                (
                    stream.codec.clone(),
                    stream.bit_depth,
                    stream.sample_rate,
                    stream.channels,
                    stream.bit_rate,
                )
            })
            .unwrap_or_default();
        JellyfinTrackQuality {
            id: self.id,
            container: self.container.unwrap_or_default(),
            codec,
            bit_depth,
            sample_rate_hz,
            channels,
            bitrate_bps,
        }
    }

    fn into_track(self) -> JellyfinTrack {
        // The FIRST audio stream. A cover embedded as `EmbeddedImage` shows up
        // in the same array, which is why this filters by type rather than
        // taking `[0]` — that would read a JPEG's 8-bit depth as the audio's.
        let audio = self
            .media_sources
            .into_iter()
            .next()
            .map(|ms| ms.media_streams)
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.kind == "Audio");
        let artist = self.artists.first().cloned().unwrap_or_default();
        let album_artist = self.album_artist.clone().unwrap_or_else(|| artist.clone());
        let genres = self
            .genres
            .into_iter()
            .map(|genre| genre.trim().to_string())
            .filter(|genre| !genre.is_empty())
            .fold(Vec::<String>::new(), |mut out, genre| {
                if !out.iter().any(|value| value.eq_ignore_ascii_case(&genre)) {
                    out.push(genre);
                }
                out
            });
        let item_image_tag = self
            .image_tags
            .iter()
            .find(|(kind, _)| kind.eq_ignore_ascii_case("primary"))
            .map(|(_, tag)| tag.trim().to_string())
            .filter(|tag| !tag.is_empty());
        JellyfinTrack {
            id: self.id,
            title: self.name,
            artist: if artist.is_empty() {
                album_artist.clone()
            } else {
                artist
            },
            album_artist,
            album: self.album.unwrap_or_default(),
            album_id: self.album_id.unwrap_or_default(),
            track_number: self.index_number,
            disc_number: self.parent_index_number,
            duration_ms: JellyfinTrack::ticks_to_ms(self.run_time_ticks.unwrap_or(0)),
            year: self.production_year,
            genre: genres.first().cloned(),
            genres,
            container: self.container.unwrap_or_default(),
            codec: audio.as_ref().and_then(|a| a.codec.clone()),
            // A lossy codec reports no bit depth. Preserved as `None` rather
            // than defaulted to 16, which would badge an MP3 as CD quality.
            bit_depth: audio.as_ref().and_then(|a| a.bit_depth),
            sample_rate_hz: audio.as_ref().and_then(|a| a.sample_rate),
            channels: audio.as_ref().and_then(|a| a.channels),
            bitrate_bps: audio.as_ref().and_then(|a| a.bit_rate),
            album_image_tag: self.album_primary_image_tag,
            item_image_tag,
            server_path: self.path,
            recording_mbid: self
                .provider_ids
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("MusicBrainzTrack"))
                .map(|(_, value)| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        }
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// A connected Jellyfin server.
///
/// Holds no credentials store and no cache — those belong to the frontend and
/// to `qbz-source` respectively (ADR-006).
#[derive(Debug, Clone)]
pub struct JellyfinClient {
    http: reqwest::Client,
    base: String,
    token: String,
    user_id: String,
    /// The install's persisted DeviceId (see [`auth_header`]). Sent with the
    /// token so the server keeps attributing requests to the session that
    /// owns it instead of rewriting that device's record.
    device_id: String,
}

/// Normalise a user-typed address: strip a trailing slash, default the scheme
/// to http (a LAN server is the overwhelming case and typing `http://` is the
/// most common omission).
pub fn normalize_base_url(input: &str) -> String {
    let s = input.trim().trim_end_matches('/');
    if s.is_empty() {
        return String::new();
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("http://{s}")
    }
}

/// The `Authorization` header every Jellyfin client must send, with or without
/// a token.
///
/// **`DeviceId` is load-bearing, and getting it wrong revokes your own token.**
/// Jellyfin keys the SESSION on it, and a second `AuthenticateByName` under the
/// same DeviceId replaces the first — the earlier token starts answering 401.
/// Measured against 10.11.11:
///
/// ```text
/// same DeviceId:      auth -> T1, auth -> T2;  T1 = 401, T2 = 200
/// different DeviceId: auth -> A,  auth -> B;   A  = 200, B  = 200
/// ```
///
/// Two obligations follow, and neither is optional:
///
/// 1. **Stable per install**, persisted like `local_plex::client_id` — not a
///    constant (two QBZ installs would then fight over one session, each
///    logging the other out) and not fresh per launch (which both litters the
///    server's devices page and drops the previous run's token).
/// 2. **Authenticate ONCE and hold the token.** Two concurrent auths leave
///    whichever finishes first holding a dead one. This is not hypothetical:
///    it is what five parallel live tests did to each other before they were
///    made to share a session.
///
/// Values are percent-encoded: the server URL-decodes every value it parses
/// (`AuthorizationContext.GetParts`), so a raw `+`, `%`, `"` or `,` would
/// otherwise arrive altered or split the header.
fn auth_header(device_id: &str, token: Option<&str>) -> String {
    let mut h = format!(
        r#"MediaBrowser Client="QBZ", Device="{}", DeviceId="{}", Version="{}""#,
        header_value(std::env::consts::OS),
        header_value(device_id),
        header_value(env!("CARGO_PKG_VERSION")),
    );
    if let Some(t) = token {
        h.push_str(&format!(r#", Token="{}""#, header_value(t)));
    }
    h
}

/// Percent-encode everything outside RFC 3986's unreserved set.
fn header_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn client() -> Result<reqwest::Client> {
    qbz_net_proxy::apply_current(reqwest::Client::builder())
        .map_err(|e| JellyfinError::Transport(e.to_string()))?
        .connect_timeout(Duration::from_secs(5))
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| JellyfinError::Transport(e.to_string()))
}

/// Reqwest's Display text contains the request URL for common failures. Server
/// addresses are private configuration and authenticated stream URLs carry a
/// token, so protocol errors retain only a coarse, non-sensitive reason.
fn transport_error(error: &reqwest::Error) -> JellyfinError {
    let reason = if error.is_timeout() {
        "request timed out"
    } else if error.is_connect() {
        "connection failed"
    } else if error.is_decode() {
        "response decoding failed"
    } else if error.is_body() {
        "response body failed"
    } else {
        "request failed"
    };
    JellyfinError::Transport(reason.to_string())
}

fn check(status: reqwest::StatusCode) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(JellyfinError::Unauthorized);
    }
    Err(JellyfinError::Status(status.as_u16()))
}

/// `GET /System/Info/Public` — no credentials required.
///
/// The settings panel's "test connection": it separates "wrong address" from
/// "wrong password" BEFORE the user is asked for one.
pub async fn probe(base_url: &str) -> Result<ServerInfo> {
    let base = normalize_base_url(base_url);
    let resp = client()?
        .get(format!("{base}/System/Info/Public"))
        .send()
        .await
        .map_err(|e| transport_error(&e))?;
    check(resp.status())?;
    resp.json::<ServerInfo>()
        .await
        .map_err(|e| JellyfinError::Decode(e.to_string()))
}

/// `POST /Users/AuthenticateByName`.
pub async fn authenticate(
    base_url: &str,
    device_id: &str,
    username: &str,
    password: &str,
) -> Result<Session> {
    let base = normalize_base_url(base_url);
    let resp = client()?
        .post(format!("{base}/Users/AuthenticateByName"))
        .header("Authorization", auth_header(device_id, None))
        .json(&serde_json::json!({ "Username": username, "Pw": password }))
        .send()
        .await
        .map_err(|e| transport_error(&e))?;
    check(resp.status()).map_err(|e| match e {
        JellyfinError::Unauthorized => JellyfinError::AuthenticationRejected,
        other => other,
    })?;
    let dto: AuthDto = resp
        .json()
        .await
        .map_err(|e| JellyfinError::Decode(e.to_string()))?;
    Ok(Session {
        access_token: dto.access_token,
        user_id: dto.user.id,
        user_name: dto.user.name,
        server_id: dto.server_id,
    })
}

/// A pending pairing request. The secret stays private and is never formatted.
pub struct QuickConnect {
    http: reqwest::Client,
    base: String,
    device_id: String,
    secret: String,
    code: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct QuickConnectDto {
    authenticated: bool,
    #[serde(default)]
    secret: String,
    #[serde(default)]
    code: String,
}
impl QuickConnect {
    pub async fn start(base_url: &str, device_id: &str) -> Result<Self> {
        let http = client()?;
        let base = normalize_base_url(base_url);
        let response = http
            .get(format!("{base}/QuickConnect/Enabled"))
            .send()
            .await
            .map_err(|e| transport_error(&e))?;
        check(response.status())?;
        let enabled: bool = response
            .json()
            .await
            .map_err(|_| JellyfinError::Decode("invalid Quick Connect availability".into()))?;
        if !enabled {
            return Err(JellyfinError::QuickConnectDisabled);
        }
        let response = http
            .post(format!("{base}/QuickConnect/Initiate"))
            .header("Authorization", auth_header(device_id, None))
            .send()
            .await
            .map_err(|e| transport_error(&e))?;
        quick_connect_check(response.status())?;
        let dto: QuickConnectDto = response
            .json()
            .await
            .map_err(|_| JellyfinError::Decode("invalid Quick Connect response".into()))?;
        if dto.secret.is_empty() || dto.code.is_empty() {
            return Err(JellyfinError::Decode(
                "missing Quick Connect secret or code".into(),
            ));
        }
        Ok(Self {
            http,
            base,
            device_id: device_id.into(),
            secret: dto.secret,
            code: dto.code,
        })
    }
    pub fn code(&self) -> &str {
        &self.code
    }
    pub async fn authorized(&self) -> Result<bool> {
        let response = self
            .http
            .get(format!("{}/QuickConnect/Connect", self.base))
            .query(&[("secret", &self.secret)])
            .send()
            .await
            .map_err(|e| transport_error(&e))?;
        quick_connect_check(response.status())?;
        let dto: QuickConnectDto = response
            .json()
            .await
            .map_err(|_| JellyfinError::Decode("invalid Quick Connect status".into()))?;
        Ok(dto.authenticated)
    }
    /// Sequential polling: no overlapping requests, bounded even if the server
    /// keeps returning pending forever. Dropping this future cancels polling.
    pub async fn wait_for_authorization(&self, limit: Duration) -> Result<Session> {
        tokio::time::timeout(limit, async {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if self.authorized().await? {
                    return self.authenticate().await;
                }
            }
        })
        .await
        .map_err(|_| JellyfinError::QuickConnectExpired)?
    }
    pub async fn authenticate(&self) -> Result<Session> {
        let response = self
            .http
            .post(format!("{}/Users/AuthenticateWithQuickConnect", self.base))
            .header("Authorization", auth_header(&self.device_id, None))
            .json(&serde_json::json!({"Secret": self.secret}))
            .send()
            .await
            .map_err(|e| transport_error(&e))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(JellyfinError::QuickConnectExpired);
        }
        check(response.status()).map_err(|error| match error {
            JellyfinError::Unauthorized => JellyfinError::AuthenticationRejected,
            other => other,
        })?;
        let dto: AuthDto = response
            .json()
            .await
            .map_err(|_| JellyfinError::Decode("invalid Quick Connect session".into()))?;
        if dto.access_token.is_empty() || dto.user.id.is_empty() {
            return Err(JellyfinError::Decode(
                "missing Quick Connect session credentials".into(),
            ));
        }
        Ok(Session {
            access_token: dto.access_token,
            user_id: dto.user.id,
            user_name: dto.user.name,
            server_id: dto.server_id,
        })
    }
}
fn quick_connect_check(status: reqwest::StatusCode) -> Result<()> {
    match status.as_u16() {
        401 => Err(JellyfinError::QuickConnectDisabled),
        404 => Err(JellyfinError::QuickConnectExpired),
        _ => check(status),
    }
}

impl JellyfinClient {
    /// Build a client over an EXISTING token (the stored-credentials path).
    ///
    /// `device_id` must be the one the token was issued under — the install's
    /// persisted id, never a fresh one (see [`auth_header`]).
    pub fn new(base_url: &str, token: &str, user_id: &str, device_id: &str) -> Result<Self> {
        Ok(Self {
            http: client()?,
            base: normalize_base_url(base_url),
            token: token.to_string(),
            user_id: user_id.to_string(),
            device_id: device_id.to_string(),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    async fn get(&self, path_and_query: &str) -> Result<reqwest::Response> {
        self.http
            .get(format!("{}{}", self.base, path_and_query))
            // THE header, never `X-Emby-Token`: see the module docs.
            .header(
                "Authorization",
                auth_header(&self.device_id, Some(&self.token)),
            )
            .send()
            .await
            .map_err(|e| transport_error(&e))
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, path_and_query: &str) -> Result<T> {
        let resp = self.get(path_and_query).await?;
        check(resp.status())?;
        decode(resp).await
    }

    /// The user's MUSIC libraries (`CollectionType == "music"`).
    ///
    /// A server also exposes a `playlists` view; it is filtered out here rather
    /// than by the caller, because "which of these is music" is a question
    /// about Jellyfin, and this crate is where Jellyfin's vocabulary lives.
    ///
    /// `GET /UserViews?userId=` is the current route (10.9+). The older
    /// `/Users/{id}/Views` is marked obsolete in 12 and liable for removal, so
    /// it is only asked when the current route does not exist (10.8).
    pub async fn music_libraries(&self) -> Result<Vec<MusicLibrary>> {
        let resp = self
            .get(&format!("/UserViews?userId={}", self.user_id))
            .await?;
        let resp = if resp.status() == reqwest::StatusCode::NOT_FOUND {
            self.get(&format!("/Users/{}/Views", self.user_id)).await?
        } else {
            resp
        };
        check(resp.status())?;
        let env: ItemsEnvelope<ViewDto> = decode(resp).await?;
        Ok(env
            .items
            .into_iter()
            .filter(|v| v.collection_type.as_deref() == Some("music"))
            .map(|v| MusicLibrary {
                id: v.id,
                name: v.name,
            })
            .collect())
    }

    /// How many audio items a library holds — one request, `Limit=0`.
    ///
    /// Worth its own call: the sweep costs ~9.3 ms per track, so a UI that
    /// wants to show progress needs the denominator before it starts.
    pub async fn track_count(&self, library_id: Option<&str>) -> Result<u64> {
        let scope = library_id
            .map(|id| format!("&parentId={id}"))
            .unwrap_or_default();
        let env: ItemsEnvelope<serde_json::Value> = self
            .get_json(&format!(
                "/Items?userId={}&IncludeItemTypes=Audio&Recursive=true&Limit=0{scope}",
                self.user_id
            ))
            .await?;
        Ok(env.total_record_count)
    }

    /// One page of essential audio metadata, without `MediaSources`.
    ///
    /// This is the first useful pass. On the measured server it avoids the
    /// per-track media-info probe that made the old first sync take 45.8 s.
    /// The returned quality fields are intentionally absent until
    /// [`track_quality`] fills them in the secondary pass.
    pub async fn essential_tracks_page(
        &self,
        library_id: Option<&str>,
        start_index: u64,
        min_date_last_saved: Option<&str>,
    ) -> Result<(Vec<JellyfinTrack>, u64)> {
        self.tracks_page_with_fields(library_id, start_index, min_date_last_saved, false)
            .await
    }

    /// Compatibility entry point for callers that explicitly need quality in
    /// the listing. The Qt catalog sync uses [`essential_tracks_page`] and the
    /// bounded [`track_quality`] path instead.
    ///
    /// `Fields=MediaSources` is what carries `BitDepth` / `SampleRate`, and it
    /// is the expensive part (§ module docs): ~4.5 s per 500-item page against
    /// the measured server. There is no cheaper way to get the quality tier, so
    /// the caller owns the cost knowingly — it is why [`track_count`] exists.
    ///
    /// `min_date_last_saved` turns the sweep into a DELTA. Jellyfin honours it
    /// (verified: a future date returns 0 items), which Plex has no equivalent
    /// for — a re-scan after the first one need not pay the full 45.8 s.
    pub async fn tracks_page(
        &self,
        library_id: Option<&str>,
        start_index: u64,
        min_date_last_saved: Option<&str>,
    ) -> Result<(Vec<JellyfinTrack>, u64)> {
        self.tracks_page_with_fields(library_id, start_index, min_date_last_saved, true)
            .await
    }

    async fn tracks_page_with_fields(
        &self,
        library_id: Option<&str>,
        start_index: u64,
        min_date_last_saved: Option<&str>,
        include_quality: bool,
    ) -> Result<(Vec<JellyfinTrack>, u64)> {
        let env: ItemsEnvelope<AudioDto> = self
            .get_json(&tracks_page_path(
                &self.user_id,
                library_id,
                start_index,
                min_date_last_saved,
                include_quality,
            ))
            .await?;
        let total = env.total_record_count;
        Ok((
            env.items.into_iter().map(AudioDto::into_track).collect(),
            total,
        ))
    }

    /// Hydrate quality for a bounded set of opaque item ids.
    ///
    /// The response is joined by id, never by position: an item may disappear
    /// between the essential pass and this request. Missing ids are simply not
    /// returned so the caller can defer them without corrupting a neighbour.
    pub async fn track_quality(&self, item_ids: &[String]) -> Result<Vec<JellyfinTrackQuality>> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        if item_ids.len() > QUALITY_BATCH_SIZE {
            return Err(JellyfinError::Decode(format!(
                "quality batch exceeds {QUALITY_BATCH_SIZE} ids"
            )));
        }
        let ids = item_ids.join(",");
        let env: ItemsEnvelope<AudioDto> = self
            .get_json(&format!(
                "/Items?userId={}&Ids={ids}&IncludeItemTypes=Audio\
                 &Limit={QUALITY_BATCH_SIZE}&Fields=MediaSources",
                self.user_id
            ))
            .await?;
        Ok(env.items.into_iter().map(AudioDto::into_quality).collect())
    }

    /// [`stream_url`] for this client's server.
    pub fn stream_url(&self, item_id: &str) -> String {
        stream_url(&self.base, &self.token, item_id)
    }

    /// [`image_url`] for this client's server.
    pub fn image_url(&self, item_id: &str, tag: Option<&str>, px: u32) -> String {
        image_url(&self.base, item_id, tag, px)
    }
}

async fn decode<T: for<'de> Deserialize<'de>>(resp: reqwest::Response) -> Result<T> {
    resp.json::<T>()
        .await
        .map_err(|e| JellyfinError::Decode(e.to_string()))
}

fn tracks_page_path(
    user_id: &str,
    library_id: Option<&str>,
    start_index: u64,
    min_date_last_saved: Option<&str>,
    include_quality: bool,
) -> String {
    let scope = library_id
        .map(|id| format!("&parentId={id}"))
        .unwrap_or_default();
    // BOTH parameters, same value. Every server version filters them
    // identically (item `DateLastSaved`), but 10.8–10.10 build the
    // `minDateLastSaved` clause against the `@MinDateLastSavedForUser` SQL
    // parameter: sent alone, 10.9/10.10 answer 500 and 10.8 evaluates NULL and
    // returns nothing. Supplying the second binds what the clause names;
    // 10.11+ simply applies the same filter twice.
    let delta = min_date_last_saved
        .map(|date| format!("&minDateLastSaved={date}&minDateLastSavedForUser={date}"))
        .unwrap_or_default();
    // ProviderIds on both: a handful of bytes per item, and it is the only
    // cross-source identity a media server can hand us.
    let fields = if include_quality {
        "Path,MediaSources,ParentId,ProductionYear,Genres,ProviderIds"
    } else {
        // A server-side filesystem path is neither useful nor safe catalog
        // metadata on a client. The compatibility path keeps exposing it, but
        // the cheap persistent-library pass does not request or store it.
        "ParentId,ProductionYear,Genres,ProviderIds"
    };
    format!(
        "/Items?userId={user_id}&IncludeItemTypes=Audio&Recursive=true\
         &Limit={PAGE_SIZE}&StartIndex={start_index}&Fields={fields}\
         &SortBy=Album,ParentIndexNumber,IndexNumber&SortOrder=Ascending\
         &EnableImages=true&ImageTypeLimit=1{scope}{delta}"
    )
}

// ---------------------------------------------------------------------------
// URL builders — PURE, and deliberately outside the client
// ---------------------------------------------------------------------------
//
// `qbz-source`'s `JellyfinSource` builds both from stored settings on paths
// where no client exists (the artwork token interpretation must not touch the
// network, by trait contract), and a `reqwest::Client` cannot even be
// CONSTRUCTED without a TLS provider installed — which a unit test does not
// have. Keeping these pure makes both true at once.

/// The BIT-PERFECT stream url for one item.
///
/// `static=true` is the whole contract: the server hands back the original file
/// bytes (md5-verified against the file on disk), with a `Content-Length` and
/// `Accept-Ranges: bytes` so QBZ's progressive feeder can Range-stream it. No
/// transcode is requested, and none may be added here — `audioCodec` /
/// `audioBitRate` on this endpoint are exactly how a bit-perfect path becomes a
/// resampled one.
///
/// The token rides in the query string because the url is handed to consumers
/// that cannot add headers (a DLNA / Chromecast renderer fetches it itself).
/// `ApiKey`, never `api_key`: the lowercase form is legacy authorization, which
/// Jellyfin 12 no longer reads (module docs). That makes this a SECRET-BEARING
/// string: never log it whole.
pub fn stream_url(base_url: &str, token: &str, item_id: &str) -> String {
    format!(
        "{}/Audio/{}/stream?static=true&ApiKey={}",
        normalize_base_url(base_url),
        item_id,
        header_value(token)
    )
}

/// The cover url for an album (or any item that has a primary image).
///
/// **No credentials.** Verified: `/Items/{id}/Images/Primary` answers 200
/// unauthenticated. That makes the url STABLE — unlike a Plex thumb, whose
/// token is rebuilt every pass — so its cache key and its fetch url can be the
/// SAME string and it can be memoized forever.
///
/// `tag` is appended when known so a re-uploaded cover busts the cache instead
/// of serving the old one until eviction.
pub fn image_url(base_url: &str, item_id: &str, tag: Option<&str>, px: u32) -> String {
    let base = normalize_base_url(base_url);
    match tag {
        Some(t) => format!("{base}/Items/{item_id}/Images/Primary?maxWidth={px}&tag={t}"),
        None => format!("{base}/Items/{item_id}/Images/Primary?maxWidth={px}"),
    }
}

/// Does this response carry the ORIGINAL bytes, or is the server transcoding?
///
/// The tell measured on 10.11.11: a direct `?static=true` answer has a
/// `Content-Length`; a transcode answers `Transfer-Encoding: chunked` with none.
/// Cheap enough to assert on every stream, and it is the only automatic defence
/// against a server-side policy silently resampling a hi-res track — which is
/// the one failure the audio contract cannot tolerate.
pub fn is_direct_response(content_length: Option<u64>, transfer_encoding: Option<&str>) -> bool {
    if let Some(te) = transfer_encoding {
        if te.eq_ignore_ascii_case("chunked") {
            return false;
        }
    }
    content_length.is_some_and(|n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_are_normalised_the_way_people_type_them() {
        assert_eq!(
            normalize_base_url("192.168.0.69:8096"),
            "http://192.168.0.69:8096"
        );
        assert_eq!(normalize_base_url("http://host:8096/"), "http://host:8096");
        assert_eq!(
            normalize_base_url(" https://jf.example.com/ "),
            "https://jf.example.com"
        );
        assert_eq!(normalize_base_url("  "), "");
    }

    #[test]
    fn the_auth_header_carries_a_token_only_when_there_is_one() {
        let without = auth_header("qbz-abc", None);
        assert!(without.contains(r#"DeviceId="qbz-abc""#));
        assert!(!without.contains("Token="));
        assert!(auth_header("qbz-abc", Some("sekrit")).contains(r#"Token="sekrit""#));
    }

    /// The server URL-decodes each value, so anything outside the unreserved
    /// set must be encoded or it arrives changed (`+` becomes a space) or ends
    /// the value early (`"`, `,`).
    #[test]
    fn auth_header_values_survive_the_servers_url_decoding() {
        let h = auth_header("id+with,odd\"chars", Some("a+b%c"));
        assert!(h.starts_with("MediaBrowser "));
        assert!(h.contains(r#"DeviceId="id%2Bwith%2Codd%22chars""#), "{h}");
        assert!(h.contains(r#"Token="a%2Bb%25c""#), "{h}");
        assert_eq!(header_value("qbz-0f3A_9.~"), "qbz-0f3A_9.~");
    }

    /// A chunked response with no `Content-Length` is the server transcoding.
    /// This is the assertion that keeps a bit-perfect path bit-perfect.
    #[test]
    fn a_chunked_response_is_never_direct() {
        assert!(is_direct_response(Some(135_919_471), None));
        assert!(!is_direct_response(None, Some("chunked")));
        assert!(!is_direct_response(None, None));
        assert!(!is_direct_response(Some(0), None));
        // A direct response that also names an identity encoding is fine.
        assert!(is_direct_response(Some(1024), Some("identity")));
    }

    /// The cover array of an item carries the EMBEDDED ARTWORK as a stream too
    /// (`mjpeg`, `BitDepth: 8`). Taking `MediaStreams[0]` would read the
    /// JPEG's depth as the audio's and badge a 24-bit FLAC as 8-bit.
    #[test]
    fn the_audio_stream_is_picked_by_type_not_by_position() {
        let json = serde_json::json!({
            "Id": "abc",
            "Name": "Harvester Of Sorrow",
            "Album": "...And Justice For All",
            "AlbumId": "alb",
            "AlbumArtist": "Metallica",
            "Artists": ["Metallica"],
            "IndexNumber": 6,
            "ParentIndexNumber": 1,
            "RunTimeTicks": 3484800000i64,
            "ProductionYear": 1988,
            "Genres": ["Thrash Metal", "Heavy Metal", "thrash metal"],
            "Container": "flac",
            "AlbumPrimaryImageTag": "748607df",
            "ImageTags": { "Primary": "disc-specific" },
            "MediaSources": [{
                "MediaStreams": [
                    { "Type": "EmbeddedImage", "Codec": "mjpeg", "BitDepth": 8 },
                    { "Type": "Audio", "Codec": "flac", "BitDepth": 24,
                      "SampleRate": 96000, "Channels": 2, "BitRate": 3120281 }
                ]
            }]
        });
        let t = serde_json::from_value::<AudioDto>(json)
            .unwrap()
            .into_track();
        assert_eq!(
            t.bit_depth,
            Some(24),
            "read the cover's depth, not the audio's"
        );
        assert_eq!(t.sample_rate_hz, Some(96000));
        assert_eq!(t.codec.as_deref(), Some("flac"));
        // 3 484 800 000 ticks / 10 000 = 348 480 ms = 348.48 s.
        assert_eq!(t.duration_ms, 348_480);
        assert_eq!(t.track_number, Some(6));
        assert_eq!(t.disc_number, Some(1));
        assert_eq!(t.genres, ["Thrash Metal", "Heavy Metal"]);
        assert_eq!(t.genre.as_deref(), Some("Thrash Metal"));
        assert_eq!(t.item_image_tag.as_deref(), Some("disc-specific"));
    }

    #[test]
    fn essential_page_omits_media_sources_but_preserves_delta_and_scope() {
        let essential = tracks_page_path(
            "user",
            Some("library"),
            250,
            Some("2026-08-23T00:00:00Z"),
            false,
        );
        assert!(essential.contains("Limit=250&StartIndex=250"));
        assert!(essential.contains("parentId=library"));
        assert!(essential.contains("minDateLastSaved=2026-08-23T00:00:00Z"));
        assert!(
            essential.contains("minDateLastSavedForUser=2026-08-23T00:00:00Z"),
            "10.8-10.10 need both delta parameters bound"
        );
        assert!(!essential.contains("MediaSources"));
        assert!(!essential.contains("Path"));

        let compatibility = tracks_page_path("user", None, 0, None, true);
        assert!(compatibility.contains("MediaSources"));
    }

    #[test]
    fn secondary_quality_is_keyed_by_item_and_uses_only_the_audio_stream() {
        let json = serde_json::json!({
            "Id": "quality-id",
            "Container": "flac",
            "MediaSources": [{ "MediaStreams": [
                { "Type": "EmbeddedImage", "Codec": "mjpeg", "BitDepth": 8 },
                { "Type": "Audio", "Codec": "flac", "BitDepth": 24,
                  "SampleRate": 192000, "Channels": 2, "BitRate": 6112000 }
            ]}]
        });
        let quality = serde_json::from_value::<AudioDto>(json)
            .unwrap()
            .into_quality();
        assert_eq!(quality.id, "quality-id");
        assert_eq!(quality.container, "flac");
        assert_eq!(quality.codec.as_deref(), Some("flac"));
        assert_eq!(quality.bit_depth, Some(24));
        assert_eq!(quality.sample_rate_hz, Some(192_000));
        assert_eq!(quality.bitrate_bps, Some(6_112_000));
    }

    #[test]
    fn essential_first_pass_metric_is_bounded_and_smaller_than_quality_payloads() {
        const TRACKS: u64 = 4_924;
        fn page(start: u64, end: u64, quality: bool) -> String {
            let mut items = Vec::new();
            for index in start..end {
                let mut item = serde_json::json!({
                    "Id": format!("item-{index:05}"),
                    "Name": format!("Track {index:05}"),
                    "Album": format!("Album {:04}", index / 10),
                    "AlbumId": format!("album-{:04}", index / 10),
                    "AlbumArtist": "Fixture Artist",
                    "Artists": ["Fixture Artist"],
                    "IndexNumber": index % 10 + 1,
                    "ParentIndexNumber": 1,
                    "RunTimeTicks": 1_800_000_000i64,
                    "ProductionYear": 2026,
                    "Genres": ["Fixture"],
                    "Container": "flac"
                });
                if quality {
                    item["MediaSources"] = serde_json::json!([{
                        "MediaStreams": [{
                            "Type": "Audio", "Codec": "flac", "BitDepth": 24,
                            "SampleRate": 96000, "Channels": 2, "BitRate": 3120000
                        }]
                    }]);
                }
                items.push(item);
            }
            serde_json::json!({ "Items": items, "TotalRecordCount": TRACKS }).to_string()
        }

        let mut essential_pages = Vec::new();
        let mut quality_pages = Vec::new();
        for start in (0..TRACKS).step_by(PAGE_SIZE as usize) {
            let end = (start + u64::from(PAGE_SIZE)).min(TRACKS);
            essential_pages.push(page(start, end, false));
            quality_pages.push(page(start, end, true));
        }
        let essential_bytes = essential_pages.iter().map(String::len).max().unwrap_or(0);
        let quality_bytes = quality_pages.iter().map(String::len).max().unwrap_or(0);
        let mut parsed = 0usize;
        let essential_started = std::time::Instant::now();
        for json in &essential_pages {
            let envelope: ItemsEnvelope<AudioDto> = serde_json::from_str(json).unwrap();
            parsed += envelope.items.len();
        }
        let essential_elapsed = essential_started.elapsed();
        let quality_started = std::time::Instant::now();
        for json in &quality_pages {
            let _: ItemsEnvelope<AudioDto> = serde_json::from_str(json).unwrap();
        }
        let quality_elapsed = quality_started.elapsed();
        assert_eq!(parsed, TRACKS as usize);
        assert!(essential_bytes < quality_bytes);
        println!(
            "H_JELLYFIN_METRIC tracks={TRACKS} pages=20 essential_max_bytes={essential_bytes} quality_max_bytes={quality_bytes} essential_parse_ms={} quality_parse_ms={}",
            essential_elapsed.as_millis(),
            quality_elapsed.as_millis(),
        );
    }

    /// MP3 reports NO bit depth. Defaulting it to 16 would badge every lossy
    /// track as CD quality, which is the one thing this app must not get wrong.
    #[test]
    fn a_lossy_track_keeps_an_absent_bit_depth() {
        let json = serde_json::json!({
            "Id": "m", "Name": "x", "Container": "mp3",
            "MediaSources": [{ "MediaStreams": [
                { "Type": "Audio", "Codec": "mp3", "SampleRate": 44100, "Channels": 2 }
            ]}]
        });
        let t = serde_json::from_value::<AudioDto>(json)
            .unwrap()
            .into_track();
        assert_eq!(t.bit_depth, None);
        assert_eq!(t.sample_rate_hz, Some(44100));
    }

    /// 17 of 500 measured rows carried no artist at all, and 75 of 4924 no
    /// track number. Neither may drop the row.
    #[test]
    fn a_row_missing_its_tags_still_maps() {
        let json = serde_json::json!({ "Id": "bare", "Name": "Untitled" });
        let t = serde_json::from_value::<AudioDto>(json)
            .unwrap()
            .into_track();
        assert_eq!(t.id, "bare");
        assert_eq!(t.artist, "");
        assert_eq!(t.track_number, None);
        assert_eq!(t.duration_ms, 0);
        assert_eq!(t.album_image_tag, None);
        assert_eq!(t.item_image_tag, None);
    }

    /// An artist-less row falls back to the ALBUM artist rather than rendering
    /// blank, and vice versa — the two fields disagree often enough on real
    /// libraries that picking one is not enough.
    #[test]
    fn artist_and_album_artist_back_each_other_up() {
        let json = serde_json::json!({
            "Id": "a", "Name": "t", "AlbumArtist": "Various Artists", "Artists": []
        });
        let t = serde_json::from_value::<AudioDto>(json)
            .unwrap()
            .into_track();
        assert_eq!(t.artist, "Various Artists");
        assert_eq!(t.album_artist, "Various Artists");
    }

    #[test]
    fn image_urls_carry_no_credentials_and_bust_on_the_tag() {
        let with = image_url("http://h:8096", "alb", Some("deadbeef"), IMAGE_PX);
        assert!(
            !with.contains("tok"),
            "a cover url must not carry the token"
        );
        assert!(with.contains("tag=deadbeef"));
        assert!(with.contains("maxWidth=256"));
        assert_eq!(
            image_url("http://h:8096", "alb", None, 1024),
            "http://h:8096/Items/alb/Images/Primary?maxWidth=1024"
        );
    }

    /// `static=true` is the bit-perfect contract. If this assertion ever has to
    /// change, the change is a resampled hi-res track.
    #[test]
    fn the_stream_url_asks_for_the_original_bytes_and_nothing_else() {
        let u = stream_url("http://h:8096/", "tok", "item42");
        assert_eq!(
            u,
            "http://h:8096/Audio/item42/stream?static=true&ApiKey=tok"
        );
        assert!(!u.contains("api_key"), "api_key is legacy authorization");
        assert!(
            !u.contains("audioCodec"),
            "a codec parameter IS a transcode"
        );
        assert!(!u.contains("audioBitRate"));
    }

    #[test]
    fn only_music_views_are_returned() {
        let json = serde_json::json!({
            "Items": [
                { "Id": "1", "Name": "Music", "CollectionType": "music" },
                { "Id": "2", "Name": "Playlists", "CollectionType": "playlists" },
                { "Id": "3", "Name": "Movies", "CollectionType": "movies" },
                { "Id": "4", "Name": "Odd", "CollectionType": null }
            ],
            "TotalRecordCount": 4
        });
        let env: ItemsEnvelope<ViewDto> = serde_json::from_value(json).unwrap();
        let music: Vec<_> = env
            .items
            .into_iter()
            .filter(|v| v.collection_type.as_deref() == Some("music"))
            .map(|v| v.name)
            .collect();
        assert_eq!(music, vec!["Music"]);
    }
}
