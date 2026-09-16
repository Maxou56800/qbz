//! Subsonic / OpenSubsonic integration — the protocol half.
//!
//! Sibling of `qbz-plex` and `qbz-jellyfin`. Frontend-agnostic: HTTP + DTOs, no
//! cache schema, no settings store, no Qt.
//!
//! One client, many servers. The same API is spoken by **Navidrome, Gonic,
//! Airsonic, Airsonic-Advanced, Astiga and Ampache**, which is why the owner's
//! scope decision was "Subsonic, not just Navidrome". Only Navidrome 0.63.2 was
//! on the bench, so everything measured below is measured *there* and the
//! places where servers are likely to differ are called out rather than
//! smoothed over.
//!
//! Measurements: `qbz-nix-docs/qt-frontend/2026-08-20-jellyfin-subsonic/01-research.md`.
//!
//! # THE TRAP: this protocol reports failure with HTTP 200
//!
//! Measured, twice, and it is the single most dangerous property of Subsonic:
//!
//! ```text
//! GET /rest/getCoverArt.view   (no credentials)
//!   -> HTTP 200, 251 bytes: <subsonic-response status="failed">…<error code="10"/>
//! GET /rest/stream.view        (no credentials)
//!   -> HTTP 200, 196 bytes: {"subsonic-response":{"status":"failed","error":{"code":10,…}}}
//! ```
//!
//! Three consequences, and all three are enforced in code below rather than
//! left to a reviewer's memory:
//!
//! 1. **The HTTP status is not the result.** `reqwest`'s `error_for_status()`
//!    passes on a failure. Every response goes through [`parse_envelope`],
//!    which checks `subsonic-response.status` before anything else.
//! 2. **A refused stream is a ~200-byte JSON blob served as the audio body.**
//!    Handed to a decoder it is garbage in the audio path. [`looks_like_audio`]
//!    exists so the caller can refuse it, and the frontend's download fallback
//!    checks Content-Type for the same reason.
//! 3. **The error format ignores `f=json`.** The no-credential `getCoverArt`
//!    case came back as **XML** even though the request asked for JSON, so the
//!    envelope parser must not assume it is parsing JSON when it fails.
//!
//! # The other trap: `path` is synthesised, not read from disk
//!
//! Navidrome reported
//! `Rob Zombie/The Lunar Injection Kool Aid Eclipse Conspiracy/01-09 - ….flac`
//! for a file that actually lives at
//! `Rob Zombie/(2021) Rob Zombie - … [FLAC] [24B-44.1kHz]/09. Rob Zombie - ….flac`
//! — different directory, different filename. The field is built from tags. It
//! is a DISPLAY STRING and must never reach a `file_path` slot; the only
//! identifier a Subsonic track has is its `id`.
//!
//! # What is good here
//!
//! - **Bit-perfect, md5-verified.** `stream.view?format=raw` returned bytes
//!   identical to the file on disk.
//! - **Quality is free.** `bitDepth` / `samplingRate` / `channelCount` are
//!   OpenSubsonic fields that ride along with every song, so a full sweep of
//!   6678 tracks took **0.81 s** — 0.122 ms per track, about 76× cheaper per
//!   track than Jellyfin's, which has to hydrate media info server-side.

use std::time::Duration;

use md5::{Digest, Md5};
use serde::Deserialize;

/// The protocol version this client claims. 1.16.1 is the last Subsonic
/// version and what every OpenSubsonic server implements; asking for it is
/// what makes `bitDepth` / `samplingRate` available.
pub const API_VERSION: &str = "1.16.1";

/// The client name every request carries (`c=`). Servers show it in their
/// session lists.
pub const CLIENT_NAME: &str = "QBZ";

/// `getAlbumList2` CAPS AT 500 regardless of what `size` asks for — measured:
/// 501 and 1000 both returned 500. Pagination via `offset` is mandatory, not
/// optional, and a caller that trusts its own `size` silently loses the tail.
pub const PAGE_SIZE: u32 = 500;

/// Cover size this client requests. **A ceiling, not a promise:** Navidrome
/// returned a 200×201 image for `size=400` because it will not upscale past
/// the embedded art. The grid must handle getting fewer pixels than it asked.
pub const IMAGE_PX: u32 = 256;

/// The larger tier, for hero / immersive slots.
pub const IMAGE_PX_LARGE: u32 = 1024;

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A Subsonic failure. `code` is the protocol's own
/// (10 = missing parameter, 40 = wrong username or password, 70 = not found).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubsonicError {
    Transport(String),
    /// `code 40` — the caller can act on this one, so it is not folded into
    /// [`SubsonicError::Api`].
    Unauthorized,
    /// `code 70`.
    NotFound(String),
    /// Any other `<error code=…>` the server returned, WITH the 200 it came in.
    Api {
        code: i64,
        message: String,
    },
    /// The body was neither a JSON envelope nor an XML one this client
    /// understands.
    Decode(String),
    /// An HTTP status that was genuinely not 2xx. Rare on this protocol —
    /// which is exactly the problem.
    Status(u16),
}

impl std::fmt::Display for SubsonicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubsonicError::Transport(e) => write!(f, "subsonic request failed: {e}"),
            SubsonicError::Unauthorized => write!(f, "subsonic rejected the credentials"),
            SubsonicError::NotFound(w) => write!(f, "subsonic has no {w}"),
            SubsonicError::Api { code, message } => {
                write!(f, "subsonic error {code}: {message}")
            }
            SubsonicError::Decode(e) => write!(f, "subsonic response not understood: {e}"),
            SubsonicError::Status(s) => write!(f, "subsonic answered HTTP {s}"),
        }
    }
}

impl std::error::Error for SubsonicError {}

type Result<T> = std::result::Result<T, SubsonicError>;

// ---------------------------------------------------------------------------
// The envelope — the safety gate
// ---------------------------------------------------------------------------

/// Pull the payload out of a `subsonic-response`, or turn its `<error>` into a
/// real [`SubsonicError`].
///
/// **Every response body must pass through here.** The protocol answers 200 on
/// failure, so this — not the HTTP status — is what decides whether a call
/// worked.
///
/// It accepts XML as well as JSON on the failure path. That is not defensive
/// padding: the measured no-credential `getCoverArt` reply was XML *despite*
/// the request carrying `f=json`, so a JSON-only parser would have reported
/// "not understood" for a perfectly clear "missing parameter: 'u'".
pub fn parse_envelope(body: &[u8]) -> Result<serde_json::Value> {
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim_start();

    // --- the XML failure shape -------------------------------------------
    if trimmed.starts_with('<') {
        if let Some(err) = xml_error(trimmed) {
            return Err(err);
        }
        return Err(SubsonicError::Decode(
            "server answered XML where JSON was requested".into(),
        ));
    }

    let root: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| SubsonicError::Decode(e.to_string()))?;
    let resp = root
        .get("subsonic-response")
        .ok_or_else(|| SubsonicError::Decode("no subsonic-response member".into()))?;

    match resp.get("status").and_then(|status| status.as_str()) {
        Some("ok") => Ok(resp.clone()),
        Some("failed") => {
            let error = resp.get("error");
            let code = error
                .and_then(|error| error.get("code"))
                .and_then(|code| code.as_i64())
                .unwrap_or(-1);
            let message = error
                .and_then(|error| error.get("message"))
                .and_then(|message| message.as_str())
                .unwrap_or("unspecified")
                .to_string();
            Err(api_error(code, message))
        }
        _ => Err(SubsonicError::Decode(
            "response has no valid status".to_string(),
        )),
    }
}

/// Map a protocol error code onto the variant a caller can act on.
fn api_error(code: i64, message: String) -> SubsonicError {
    match code {
        40 | 41 => SubsonicError::Unauthorized,
        70 => SubsonicError::NotFound(message),
        _ => SubsonicError::Api { code, message },
    }
}

/// Dig `code`/`message` out of the XML failure shape without pulling in an XML
/// parser: the document is one flat `<error .../>` element and the two
/// attributes are all this crate needs from it.
fn xml_error(xml: &str) -> Option<SubsonicError> {
    let at = xml.find("<error ")?;
    let tail = &xml[at..];
    let attr = |name: &str| -> Option<String> {
        let key = format!("{name}=\"");
        let i = tail.find(&key)? + key.len();
        let rest = &tail[i..];
        let j = rest.find('"')?;
        Some(rest[..j].to_string())
    };
    let code = attr("code")?.parse::<i64>().ok()?;
    let message = attr("message").unwrap_or_else(|| "unspecified".into());
    // The XML escapes apostrophes, which show up in the common "missing
    // parameter: 'u'" message.
    let message = message
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&amp;", "&");
    Some(api_error(code, message))
}

/// Does this response body plausibly carry AUDIO rather than an error envelope?
///
/// Necessary because a refused `stream.view` answers **HTTP 200** with a
/// ~200-byte JSON error, and that would otherwise be handed to the decoder.
/// Two independent checks, because either alone has a hole: a content type can
/// be absent or wrong, and a very short body can be a legitimate (tiny) file in
/// theory — together they are decisive for anything a music server serves.
pub fn looks_like_audio(content_type: Option<&str>, byte_len: usize) -> bool {
    if let Some(ct) = content_type {
        let ct = ct.to_ascii_lowercase();
        if ct.starts_with("application/json") || ct.starts_with("text/") || ct.contains("xml") {
            return false;
        }
    }
    // Every measured error envelope was under 300 bytes; the smallest real
    // track on the bench is orders of magnitude larger.
    byte_len >= 4096
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// Subsonic token auth: `t = md5(password + salt)`, sent with the salt.
///
/// The salt is chosen by the CLIENT and travels in clear, so it is not a
/// secret — its job is to keep the same password from hashing to the same token
/// on two different installs.
///
/// **QBZ pins one salt per install rather than rolling it per request.** The
/// protocol allows either, and rolling it looks more careful, but it would make
/// every cover URL unique per request: the artwork cache keys on the URL, so a
/// rolling salt re-downloads every cover on every pass. A fixed salt costs
/// nothing here — the password is not transmitted either way, and an attacker
/// who can read the query string can read the token whichever salt produced it.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub username: String,
    /// `md5(password + salt)`, hex, lowercase.
    pub token: String,
    pub salt: String,
}

impl Credentials {
    /// Derive the token for `password` under `salt`.
    pub fn new(username: &str, password: &str, salt: &str) -> Self {
        let mut h = Md5::new();
        h.update(password.as_bytes());
        h.update(salt.as_bytes());
        Self {
            username: username.to_string(),
            token: format!("{:x}", h.finalize()),
            salt: salt.to_string(),
        }
    }

    /// The auth + protocol query parameters every request carries.
    ///
    /// `f=json` is included even though the failure path may answer XML anyway
    /// (see [`parse_envelope`]) — the SUCCESS path honours it, and that is the
    /// path that has to be parsed.
    pub fn query(&self) -> String {
        format!(
            "u={}&t={}&s={}&v={}&c={}&f=json",
            urlencode(&self.username),
            self.token,
            urlencode(&self.salt),
            API_VERSION,
            CLIENT_NAME
        )
    }
}

/// Percent-encode the characters that would break a query string. Deliberately
/// minimal — a username or salt is not a URL path, and pulling in a full
/// encoder for two fields is more surface than it saves.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// What `ping.view` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// e.g. `navidrome`, `gonic`, `airsonic`.
    pub kind: String,
    pub version: String,
    /// The server's own build string, when it offers one.
    pub server_version: Option<String>,
    /// True when the server implements the OpenSubsonic extensions — which is
    /// what makes `bitDepth` / `samplingRate` available. A plain Subsonic
    /// server answers without them and every track's quality tier degrades to
    /// "unknown", so this flag is worth surfacing in the settings panel.
    pub open_subsonic: bool,
}

/// One music folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MusicFolder {
    pub id: String,
    pub name: String,
}

/// One album-list row. Its `coverArt` token is the collection cover and must
/// not be inferred from a song token: Navidrome can expose a separate cover
/// for every disc inside the same album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsonicAlbum {
    pub id: String,
    pub cover_art: Option<String>,
}

/// One song, flattened into the shape the cache stores. Field names are QBZ's.
#[derive(Debug, Clone, PartialEq)]
pub struct SubsonicTrack {
    /// The ONLY identifier. Opaque — never parse it, never construct it.
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub album_id: String,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub duration_ms: u64,
    pub year: Option<u32>,
    /// The OpenSubsonic song payload currently exposes one genre. Keep it as
    /// a collection so downstream models do not collapse multi-genre sources.
    pub genres: Vec<String>,
    pub genre: Option<String>,
    /// `flac`, `mp3`, … as the server names it.
    pub suffix: String,
    pub content_type: Option<String>,
    /// `None` for lossy. The wire reports **0** for that case (Jellyfin reports
    /// null); both mean "not applicable" and both are folded to `None` here so
    /// downstream sees one vocabulary.
    pub bit_depth: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u32>,
    /// kbps, as the protocol reports it.
    pub bitrate_kbps: Option<u32>,
    /// The OPAQUE cover-art id (`al-<albumId>_<hash>` for an album,
    /// `dc-<albumId>:<disc>_<n>` for a track). Store it; never build one.
    pub cover_art: Option<String>,
    /// File size in bytes, as reported. Useful as a sanity check against a
    /// stream's `Content-Length`.
    pub size: Option<u64>,
    /// OpenSubsonic `musicBrainzId` (recording) and the first `isrc`, when
    /// the server exposes them. Cross-source identity for the listen log.
    pub recording_mbid: Option<String>,
    pub isrc: Option<String>,
}

#[derive(Deserialize)]
struct SongDto {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    album: Option<String>,
    #[serde(default, rename = "albumId")]
    album_id: Option<String>,
    #[serde(default, rename = "displayAlbumArtist")]
    display_album_artist: Option<String>,
    #[serde(default)]
    track: Option<u32>,
    #[serde(default, rename = "discNumber")]
    disc_number: Option<u32>,
    /// SECONDS on the wire.
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    genre: Option<String>,
    #[serde(default)]
    suffix: Option<String>,
    #[serde(default, rename = "contentType")]
    content_type: Option<String>,
    #[serde(default, rename = "bitDepth")]
    bit_depth: Option<u32>,
    #[serde(default, rename = "samplingRate")]
    sampling_rate: Option<u32>,
    #[serde(default, rename = "channelCount")]
    channel_count: Option<u32>,
    #[serde(default, rename = "bitRate")]
    bit_rate: Option<u32>,
    #[serde(default, rename = "coverArt")]
    cover_art: Option<String>,
    #[serde(default)]
    size: Option<u64>,
    /// OpenSubsonic: the MusicBrainz RECORDING id of the song.
    #[serde(default, rename = "musicBrainzId")]
    music_brainz_id: Option<String>,
    /// OpenSubsonic: ISRCs, a list (a recording can carry several).
    #[serde(default)]
    isrc: Vec<String>,
}

impl From<SongDto> for SubsonicTrack {
    fn from(s: SongDto) -> Self {
        let artist = s.artist.unwrap_or_default();
        let album_artist = s.display_album_artist.unwrap_or_else(|| artist.clone());
        let genre = s.genre.filter(|genre| !genre.trim().is_empty());
        let genres = genre.iter().cloned().collect();
        SubsonicTrack {
            id: s.id,
            title: s.title,
            artist: if artist.is_empty() {
                album_artist.clone()
            } else {
                artist
            },
            album_artist,
            album: s.album.unwrap_or_default(),
            album_id: s.album_id.unwrap_or_default(),
            track_number: s.track,
            disc_number: s.disc_number,
            duration_ms: s.duration.unwrap_or(0) * 1000,
            year: s.year,
            genre,
            genres,
            suffix: s.suffix.unwrap_or_default(),
            content_type: s.content_type,
            // 0 means "not applicable" on this wire, not "zero bits".
            bit_depth: s.bit_depth.filter(|d| *d > 0),
            sample_rate_hz: s.sampling_rate.filter(|r| *r > 0),
            channels: s.channel_count.filter(|c| *c > 0),
            bitrate_kbps: s.bit_rate.filter(|b| *b > 0),
            cover_art: s.cover_art.filter(|c| !c.is_empty()),
            recording_mbid: s
                .music_brainz_id
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            isrc: s
                .isrc
                .into_iter()
                .map(|v| v.trim().to_string())
                .find(|v| !v.is_empty()),
            size: s.size,
        }
    }
}

/// How this client enumerated a library, so the caller can log which path a
/// given server actually supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepMode {
    /// `search3` with an empty query: 14 requests for 6678 tracks on the bench.
    Search3,
    /// `getAlbumList2` + `getAlbum` per album: 675 requests for the same
    /// library. Portable, and the fallback when the fast path is not honoured.
    PerAlbum,
}

/// The server's own media-library scan state (`getScanStatus`). `count` is
/// the number of items processed so far; the Subsonic protocol exposes no
/// total, so callers must not turn it into a percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanStatus {
    pub scanning: bool,
    pub count: u64,
}

fn scan_status_of(response: &serde_json::Value) -> Result<ScanStatus> {
    let status = response
        .get("scanStatus")
        .ok_or_else(|| SubsonicError::Decode("response has no scanStatus member".into()))?;
    let scanning = status
        .get("scanning")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| SubsonicError::Decode("scanStatus has no scanning flag".into()))?;
    let count = status
        .get("count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Ok(ScanStatus { scanning, count })
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// A connected Subsonic-compatible server.
#[derive(Debug, Clone)]
pub struct SubsonicClient {
    http: reqwest::Client,
    base: String,
    creds: Credentials,
}

/// Normalise a user-typed address and append `/rest` if it is not there.
///
/// People paste the address of the web UI, which is the server root; the API
/// lives one segment down. Doing it here rather than in the settings panel
/// means every caller gets it, including the ones written later.
pub fn normalize_base_url(input: &str) -> String {
    let s = input.trim().trim_end_matches('/');
    if s.is_empty() {
        return String::new();
    }
    let with_scheme = if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("http://{s}")
    };
    if with_scheme.ends_with("/rest") {
        with_scheme
    } else {
        format!("{with_scheme}/rest")
    }
}

fn client() -> Result<reqwest::Client> {
    qbz_net_proxy::apply_current(reqwest::Client::builder())
        .map_err(|e| SubsonicError::Transport(e.to_string()))?
        .connect_timeout(Duration::from_secs(5))
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| transport_error(&e))
}

/// Reqwest includes the full request URL in common error strings. Every
/// Subsonic URL carries the auth token, username, and salt in its query, so a
/// transport failure must retain only a coarse non-sensitive reason.
fn transport_error(error: &reqwest::Error) -> SubsonicError {
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
    SubsonicError::Transport(reason.to_string())
}

impl SubsonicClient {
    pub fn new(base_url: &str, creds: Credentials) -> Result<Self> {
        Ok(Self {
            http: client()?,
            base: normalize_base_url(base_url),
            creds,
        })
    }

    /// The `/rest` base, e.g. `http://host:4533/rest`.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn credentials(&self) -> &Credentials {
        &self.creds
    }

    /// GET one endpoint and return the unwrapped `subsonic-response`.
    async fn get(&self, endpoint: &str, extra: &str) -> Result<serde_json::Value> {
        let url = format!("{}/{}?{}{}", self.base, endpoint, self.creds.query(), extra);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| transport_error(&e))?;
        // Checked, but NOT trusted: a 200 here says nothing about success.
        let status = resp.status();
        let body = resp.bytes().await.map_err(|e| transport_error(&e))?;
        match parse_envelope(&body) {
            Ok(v) if status.is_success() => Ok(v),
            Ok(_) => Err(SubsonicError::Status(status.as_u16())),
            // A genuine non-2xx with an unparseable body is worth reporting as
            // the status it was, rather than as "not understood".
            Err(SubsonicError::Decode(d)) if !status.is_success() => {
                let _ = d;
                Err(SubsonicError::Status(status.as_u16()))
            }
            Err(e) => Err(e),
        }
    }

    /// `ping.view` — the credentials check AND the OpenSubsonic probe.
    pub async fn ping(&self) -> Result<ServerInfo> {
        let r = self.get("ping.view", "").await?;
        Ok(ServerInfo {
            kind: r
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("subsonic")
                .to_string(),
            version: r
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or(API_VERSION)
                .to_string(),
            server_version: r
                .get("serverVersion")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            open_subsonic: r
                .get("openSubsonic")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }

    /// `getScanStatus.view` — the server's own library refresh, distinct from
    /// QBZ copying that catalog into its local cache. Navidrome implements the
    /// standard endpoint and may expose incomplete/disabled rows while this is
    /// true, so callers should defer an authoritative QBZ sweep.
    pub async fn scan_status(&self) -> Result<ScanStatus> {
        let response = self.get("getScanStatus.view", "").await?;
        scan_status_of(&response)
    }

    /// `getMusicFolders.view`.
    pub async fn music_folders(&self) -> Result<Vec<MusicFolder>> {
        let r = self.get("getMusicFolders.view", "").await?;
        let arr = r
            .get("musicFolders")
            .and_then(|m| m.get("musicFolder"))
            .and_then(|f| f.as_array())
            .cloned()
            .unwrap_or_default();
        arr.into_iter()
            .map(|v| {
                // `id` is an INTEGER here and a STRING elsewhere in the same
                // protocol. Accept both rather than pick a side.
                let id = v.get("id").map(json_id).ok_or_else(|| {
                    SubsonicError::Decode("music folder row has no id".to_string())
                })?;
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                Ok(MusicFolder { id, name })
            })
            .collect()
    }

    /// One page of the FAST sweep: `search3` with an empty query.
    ///
    /// Measured on Navidrome: 500 songs in 0.095 s, paginating correctly at
    /// `songOffset=6000`. Whether an empty query means "everything" is the
    /// behaviour most likely to differ across servers, so a caller must be
    /// prepared for this to return nothing on the first page and fall back to
    /// [`SubsonicClient::album_ids`] + [`SubsonicClient::album_tracks`].
    pub async fn search_page(&self, offset: u32) -> Result<Vec<SubsonicTrack>> {
        let r = self
            .get(
                "search3.view",
                &format!(
                    "&query=%22%22&artistCount=0&albumCount=0&songCount={PAGE_SIZE}&songOffset={offset}"
                ),
            )
            .await?;
        songs_of(r.get("searchResult3"))
    }

    /// One page of albums, including their collection artwork tokens.
    ///
    /// `size` is capped at 500 by the server whatever is asked, so this always
    /// pages.
    pub async fn album_page(&self, offset: u32) -> Result<Vec<SubsonicAlbum>> {
        let r = self
            .get(
                "getAlbumList2.view",
                &format!("&type=alphabeticalByName&size={PAGE_SIZE}&offset={offset}"),
            )
            .await?;
        albums_of(r.get("albumList2"))
    }

    /// Compatibility projection for callers that only need identifiers.
    pub async fn album_ids(&self, offset: u32) -> Result<Vec<String>> {
        self.album_page(offset)
            .await
            .map(|albums| albums.into_iter().map(|album| album.id).collect())
    }

    /// The tracks of one album (`getAlbum.view`).
    pub async fn album_tracks(&self, album_id: &str) -> Result<Vec<SubsonicTrack>> {
        let r = self
            .get("getAlbum.view", &format!("&id={}", urlencode(album_id)))
            .await?;
        songs_of(r.get("album"))
    }

    /// Which sweep this server supports, decided by ASKING rather than by
    /// checking its name.
    ///
    /// The fast path is the Navidrome behaviour and the OpenSubsonic spec's
    /// intent, but Gonic and Airsonic were never on the bench. Probing one page
    /// costs ~100 ms and turns "we think this works" into "this works here".
    pub async fn detect_sweep_mode(&self) -> SweepMode {
        match self.search_page(0).await {
            Ok(rows) if !rows.is_empty() => SweepMode::Search3,
            _ => SweepMode::PerAlbum,
        }
    }

    /// The BIT-PERFECT stream url.
    ///
    /// `format=raw` is the explicit contract. The bare `stream.view` happened to
    /// return raw bytes on the bench, but only because that server had no
    /// transcoding policy for this client — that is CONFIGURATION, not a
    /// guarantee, and relying on it is how a hi-res track quietly becomes an
    /// mp3. `maxBitRate` must never be added here.
    ///
    /// Carries credentials in the query string: SECRET-BEARING, never log whole.
    pub fn stream_url(&self, track_id: &str) -> String {
        stream_url(&self.base, &self.creds, track_id)
    }

    /// The cover url for a `coverArt` id.
    pub fn cover_url(&self, cover_art: &str, px: u32) -> String {
        cover_url(&self.base, &self.creds, cover_art, px)
    }
}

// ---------------------------------------------------------------------------
// URL builders — PURE (see `qbz-jellyfin` for why these are free functions)
// ---------------------------------------------------------------------------

/// See [`SubsonicClient::stream_url`].
pub fn stream_url(base_url: &str, creds: &Credentials, track_id: &str) -> String {
    format!(
        "{}/stream.view?{}&id={}&format=raw",
        normalize_base_url(base_url),
        creds.query(),
        urlencode(track_id)
    )
}

/// See [`SubsonicClient::cover_url`].
///
/// Unlike Jellyfin's, this URL carries credentials — so the caller must key its
/// artwork cache on the opaque `coverArt` id, NOT on this string.
pub fn cover_url(base_url: &str, creds: &Credentials, cover_art: &str, px: u32) -> String {
    format!(
        "{}/getCoverArt.view?{}&id={}&size={}",
        normalize_base_url(base_url),
        creds.query(),
        urlencode(cover_art),
        px
    )
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// `id` is an integer in `getMusicFolders` and a string almost everywhere else.
fn json_id(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string().trim_matches('"').to_string(),
    }
}

/// Pull a `song` array out of whichever container holds it.
fn songs_of(container: Option<&serde_json::Value>) -> Result<Vec<SubsonicTrack>> {
    let Some(songs) = container
        .and_then(|c| c.get("song"))
        .and_then(|s| s.as_array())
    else {
        return Ok(Vec::new());
    };
    songs
        .iter()
        .map(|song| {
            serde_json::from_value::<SongDto>(song.clone())
                .map(SubsonicTrack::from)
                .map_err(|error| SubsonicError::Decode(error.to_string()))
        })
        .collect()
}

fn albums_of(container: Option<&serde_json::Value>) -> Result<Vec<SubsonicAlbum>> {
    let Some(albums) = container
        .and_then(|album_list| album_list.get("album"))
        .and_then(|albums| albums.as_array())
    else {
        return Ok(Vec::new());
    };
    albums
        .iter()
        .map(|album| {
            let id = album
                .get("id")
                .map(json_id)
                .ok_or_else(|| SubsonicError::Decode("album row has no id".to_string()))?;
            let cover_art = album
                .get("coverArt")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            Ok(SubsonicAlbum { id, cover_art })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_get_a_scheme_and_the_rest_segment() {
        assert_eq!(
            normalize_base_url("192.168.0.69:4533"),
            "http://192.168.0.69:4533/rest"
        );
        assert_eq!(normalize_base_url("http://h:4533/"), "http://h:4533/rest");
        // Already pointed at the API: not doubled.
        assert_eq!(
            normalize_base_url("http://h:4533/rest"),
            "http://h:4533/rest"
        );
        assert_eq!(normalize_base_url(""), "");
    }

    /// The known-good pair from the bench doc: password `qbz-navidrome`,
    /// salt `9dafc0` -> token `9874f6e89692e31267ff84eb5c2d5745`. If this
    /// assertion ever fails, the auth is broken in a way no live test would
    /// explain clearly.
    #[test]
    fn the_token_is_md5_of_password_then_salt() {
        let c = Credentials::new("admin", "qbz-navidrome", "9dafc0");
        assert_eq!(c.token, "9874f6e89692e31267ff84eb5c2d5745");
        assert!(c.query().contains("u=admin"));
        assert!(c.query().contains("s=9dafc0"));
        assert!(c.query().contains("f=json"));
        // The PASSWORD itself must never appear in a query string.
        assert!(!c.query().contains("qbz-navidrome"));
    }

    #[test]
    fn usernames_and_salts_are_percent_encoded() {
        let c = Credentials::new("user name+&", "p", "sa lt");
        assert!(c.query().contains("u=user%20name%2B%26"));
        assert!(c.query().contains("s=sa%20lt"));
    }

    // ── THE trap: failure arrives as HTTP 200 ──────────────────────────────

    /// The measured JSON failure body, verbatim.
    #[test]
    fn a_json_failure_envelope_is_an_error_not_a_payload() {
        let body = br#"{"subsonic-response":{"status":"failed","version":"1.16.1","type":"navidrome","error":{"code":10,"message":"missing parameter: 'u'"}}}"#;
        match parse_envelope(body) {
            Err(SubsonicError::Api { code, message }) => {
                assert_eq!(code, 10);
                assert!(message.contains("missing parameter"));
            }
            other => panic!("expected an Api error, got {other:?}"),
        }
    }

    /// The measured XML failure body — returned even though the request asked
    /// for JSON. A JSON-only parser reports "not understood" for a perfectly
    /// clear error.
    #[test]
    fn an_xml_failure_envelope_is_understood_despite_f_json() {
        let body = br#"<subsonic-response xmlns="http://subsonic.org/restapi" status="failed" version="1.16.1" type="navidrome"><error code="10" message="missing parameter: &#39;u&#39;"></error></subsonic-response>"#;
        match parse_envelope(body) {
            Err(SubsonicError::Api { code, message }) => {
                assert_eq!(code, 10);
                assert_eq!(message, "missing parameter: 'u'");
            }
            other => panic!("expected an Api error, got {other:?}"),
        }
    }

    /// Code 40 is actionable — the caller re-authenticates — so it does not
    /// hide inside the generic variant.
    #[test]
    fn wrong_credentials_are_their_own_variant() {
        let body = br#"{"subsonic-response":{"status":"failed","error":{"code":40,"message":"Wrong username or password"}}}"#;
        assert_eq!(parse_envelope(body), Err(SubsonicError::Unauthorized));
        let missing = br#"{"subsonic-response":{"status":"failed","error":{"code":70,"message":"Song not found"}}}"#;
        assert!(matches!(
            parse_envelope(missing),
            Err(SubsonicError::NotFound(_))
        ));
    }

    #[test]
    fn a_successful_envelope_unwraps_to_its_payload() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1","musicFolders":{"musicFolder":[{"id":1,"name":"Music Library"}]}}}"#;
        let v = parse_envelope(body).expect("ok envelope");
        assert_eq!(v["musicFolders"]["musicFolder"][0]["name"], "Music Library");
    }

    #[test]
    fn scan_status_preserves_server_progress_without_inventing_a_total() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","scanStatus":{"scanning":true,"count":5512}}}"#;
        let response = parse_envelope(body).expect("ok envelope");
        assert_eq!(
            scan_status_of(&response).unwrap(),
            ScanStatus {
                scanning: true,
                count: 5_512,
            }
        );
    }

    #[test]
    fn malformed_scan_status_cannot_claim_the_server_is_idle() {
        let response = serde_json::json!({ "scanStatus": { "count": 12 } });
        assert!(matches!(
            scan_status_of(&response),
            Err(SubsonicError::Decode(_))
        ));
    }

    #[test]
    fn an_envelope_without_a_valid_status_cannot_authorize_empty_results() {
        let body = br#"{"subsonic-response":{"searchResult3":{}}}"#;
        assert!(matches!(
            parse_envelope(body),
            Err(SubsonicError::Decode(_))
        ));
    }

    /// A refused stream is ~200 bytes of JSON served as the audio body. This is
    /// the guard that keeps it out of the decoder.
    #[test]
    fn an_error_body_never_looks_like_audio() {
        assert!(!looks_like_audio(Some("application/json"), 196));
        assert!(!looks_like_audio(Some("text/xml"), 251));
        assert!(!looks_like_audio(None, 196));
        // A real track.
        assert!(looks_like_audio(Some("audio/flac"), 13_445_275));
        // Right type, absurd length: still refused.
        assert!(!looks_like_audio(Some("audio/flac"), 120));
    }

    // ── Mapping ────────────────────────────────────────────────────────────

    #[test]
    fn album_listing_keeps_collection_art_separate_from_song_art() {
        let payload = serde_json::json!({
            "album": [
                {"id": "box", "coverArt": "al-box-cover"},
                {"id": 42}
            ]
        });
        let albums = albums_of(Some(&payload)).unwrap();
        assert_eq!(albums[0].id, "box");
        assert_eq!(albums[0].cover_art.as_deref(), Some("al-box-cover"));
        assert_eq!(albums[1].id, "42");
        assert_eq!(albums[1].cover_art, None);
    }

    /// The measured hi-res row. `duration` is SECONDS on the wire and
    /// milliseconds in QBZ, which is exactly the kind of unit slip that shows
    /// up as a progress bar running 1000× too fast.
    #[test]
    fn a_hires_song_maps_with_its_quality_and_a_ms_duration() {
        let json = serde_json::json!({
            "id": "dRnm19VjnLhA30hExxaCGA",
            "title": "Matilda Mother",
            "artist": "Pink Floyd",
            "album": "The Piper at the Gates of Dawn",
            "albumId": "alb1",
            "displayAlbumArtist": "Pink Floyd",
            "track": 3, "discNumber": 1, "duration": 188, "year": 1967,
            "genre": "Psychedelic Rock",
            "suffix": "flac", "contentType": "audio/flac",
            "bitDepth": 24, "samplingRate": 192000, "channelCount": 2,
            "coverArt": "al-alb1_59fec8ff", "size": 137389889u64
        });
        let t: SubsonicTrack = serde_json::from_value::<SongDto>(json).unwrap().into();
        assert_eq!(t.bit_depth, Some(24));
        assert_eq!(t.sample_rate_hz, Some(192_000));
        assert_eq!(t.duration_ms, 188_000, "seconds were not converted to ms");
        assert_eq!(t.cover_art.as_deref(), Some("al-alb1_59fec8ff"));
        assert_eq!(t.size, Some(137_389_889));
        assert_eq!(t.genres, ["Psychedelic Rock"]);
        assert_eq!(t.genre.as_deref(), Some("Psychedelic Rock"));
    }

    /// The wire reports **0** for a lossy track's bit depth (Jellyfin reports
    /// null). Left as 0 it would badge every MP3 as a 0-bit oddity or, worse,
    /// be read as "known". Both sentinels fold to None.
    #[test]
    fn a_zero_bit_depth_means_not_applicable_not_zero() {
        let json = serde_json::json!({
            "id": "x", "title": "Dime", "suffix": "mp3",
            "bitDepth": 0, "samplingRate": 44100, "channelCount": 2, "bitRate": 160
        });
        let t: SubsonicTrack = serde_json::from_value::<SongDto>(json).unwrap().into();
        assert_eq!(t.bit_depth, None);
        assert_eq!(t.sample_rate_hz, Some(44100));
        assert_eq!(t.bitrate_kbps, Some(160));
    }

    #[test]
    fn a_song_missing_everything_optional_still_maps() {
        let json = serde_json::json!({ "id": "bare" });
        let t: SubsonicTrack = serde_json::from_value::<SongDto>(json).unwrap().into();
        assert_eq!(t.id, "bare");
        assert_eq!(t.duration_ms, 0);
        assert_eq!(t.cover_art, None);
        assert_eq!(t.track_number, None);
    }

    #[test]
    fn one_malformed_song_rejects_the_whole_authoritative_page() {
        let page = serde_json::json!({
            "song": [
                { "id": "valid", "title": "Valid" },
                { "title": "Missing identity" }
            ]
        });
        assert!(matches!(
            songs_of(Some(&page)),
            Err(SubsonicError::Decode(_))
        ));
    }

    #[test]
    fn paged_sweep_metric_covers_every_id_once() {
        const TRACKS: u32 = 6_678;
        let mut pages = Vec::new();
        for start in (0..TRACKS).step_by(PAGE_SIZE as usize) {
            let end = start.saturating_add(PAGE_SIZE).min(TRACKS);
            let songs = (start..end)
                .map(|index| {
                    serde_json::json!({
                        "id": format!("song-{index:05}"),
                        "title": format!("Track {index:05}"),
                        "artist": "Fixture Artist",
                        "album": format!("Album {:04}", index / 10),
                        "albumId": format!("album-{:04}", index / 10),
                        "duration": 180,
                        "suffix": "flac",
                        "contentType": "audio/flac",
                        "bitDepth": 24,
                        "samplingRate": 96000,
                        "channelCount": 2,
                        "bitRate": 3120
                    })
                })
                .collect::<Vec<_>>();
            pages.push(serde_json::json!({ "song": songs }).to_string());
        }

        let max_bytes = pages.iter().map(String::len).max().unwrap_or(0);
        let started = std::time::Instant::now();
        let mut ids = std::collections::HashSet::new();
        for json in &pages {
            let page: serde_json::Value = serde_json::from_str(json).unwrap();
            for track in songs_of(Some(&page)).unwrap() {
                assert!(ids.insert(track.id), "fixture emitted a duplicate id");
            }
        }
        let elapsed = started.elapsed();
        assert_eq!(pages.len(), 14);
        assert_eq!(ids.len(), TRACKS as usize);
        println!(
            "H_SUBSONIC_METRIC tracks={TRACKS} pages={} max_bytes={max_bytes} parse_ms={}",
            pages.len(),
            elapsed.as_millis(),
        );
    }

    /// `id` is an integer in `getMusicFolders` and a string elsewhere. Picking
    /// one shape drops the other silently.
    #[test]
    fn ids_are_accepted_as_both_numbers_and_strings() {
        assert_eq!(json_id(&serde_json::json!(1)), "1");
        assert_eq!(json_id(&serde_json::json!("al-abc")), "al-abc");
    }

    // ── URLs ───────────────────────────────────────────────────────────────

    /// `format=raw` is the bit-perfect contract, and `maxBitRate` is how it
    /// would be lost.
    #[test]
    fn the_stream_url_demands_raw_and_never_caps_the_bitrate() {
        let c = Credentials::new("admin", "pw", "abc");
        let u = stream_url("http://h:4533", &c, "trk1");
        assert!(u.starts_with("http://h:4533/rest/stream.view?"));
        assert!(u.contains("&format=raw"), "raw is the contract: {u}");
        assert!(!u.contains("maxBitRate"), "a bitrate cap IS a transcode");
        assert!(u.contains("&id=trk1"));
        assert!(!u.contains("pw"), "the password reached the url");
    }

    /// The cover url carries credentials, which is precisely why the CACHE KEY
    /// must be the opaque coverArt id and not this string.
    #[test]
    fn the_cover_url_carries_credentials_and_a_size_ceiling() {
        let c = Credentials::new("admin", "pw", "abc");
        let u = cover_url("http://h:4533", &c, "al-abc_1:2", IMAGE_PX);
        assert!(u.contains("getCoverArt.view"));
        assert!(
            u.contains("t="),
            "an unauthenticated cover request is refused"
        );
        assert!(u.contains("size=256"));
        // The id is opaque and can contain `:` — it must be encoded, not pasted.
        assert!(u.contains("id=al-abc_1%3A2"), "{u}");
    }
}
