//! Plex LAN-only POC integration.
//!
//! This module intentionally avoids transcoding endpoints and uses `/library/parts/.../file...`
//! so playback uses original media bytes served by Plex Media Server.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

const MAX_PLEX_TRACK_PAGE_BYTES: usize = 32 * 1024 * 1024;
const DEFAULT_PLEX_TRACK_PAGE_SIZE: u64 = 250;
/// Album metadata is LAN-local and cheap, but a missing cover must not turn
/// into one request on every boot forever. Positive and negative results are
/// both rechecked weekly so a cover added later in Plex eventually appears.
pub const PLEX_ALBUM_ARTWORK_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexServerInfo {
    pub friendly_name: Option<String>,
    pub version: Option<String>,
    pub machine_identifier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexMusicSection {
    pub key: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexTrack {
    pub rating_key: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
    /// Track/item thumb. Plex may expose a disc-specific cover here.
    pub artwork_path: Option<String>,
    /// Parent album thumb, kept separately so item art can win without losing
    /// a stable collection fallback.
    #[serde(default)]
    pub collection_artwork_path: Option<String>,
    pub part_key: Option<String>,
    pub container: Option<String>,
    pub codec: Option<String>,
    pub channels: Option<u32>,
    pub bitrate_kbps: Option<u32>,
    pub sampling_rate_hz: Option<u32>,
    pub bit_depth: Option<u32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub year: Option<u32>,
    /// Every `<Genre>` Plex supplied. `genre` remains the compatibility
    /// primary value for older callers and cache rows.
    pub genres: Vec<String>,
    pub genre: Option<String>,
    /// `parentRatingKey` — the Plex album this track belongs to. Distinct per
    /// physical album/edition, so it separates two albums that share the same
    /// title+artist (which `album_key`, a title+artist hash, collapses into one).
    pub parent_rating_key: Option<String>,
    /// MusicBrainz recording id from the item's `<Guid id="mbid://…"/>`
    /// child (requires `includeGuids=1`; only the Plex Music agent writes
    /// it). `None` otherwise — never synthesised.
    #[serde(default)]
    pub recording_mbid: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PlexTrackPage {
    pub tracks: Vec<PlexTrack>,
    pub offset: u64,
    pub response_size: u64,
    pub total_size: u64,
}

impl PlexTrackPage {
    pub fn next_start(&self) -> u64 {
        self.offset.saturating_add(self.response_size)
    }

    pub fn has_more(&self) -> bool {
        self.next_start() < self.total_size
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexSectionSyncState {
    pub section_key: String,
    pub generation: u64,
    pub next_start: u64,
    pub total_size: Option<u64>,
    pub observed_rows: u64,
    pub resumed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexPlayResult {
    pub rating_key: String,
    pub part_key: String,
    pub part_url: String,
    pub bytes: usize,
    pub direct_play_confirmed: bool,
    pub content_type: Option<String>,
    pub sampling_rate_hz: Option<u32>,
    pub bit_depth: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct PlexResolvedMedia {
    pub rating_key: String,
    pub playback_id: u64,
    pub part_key: String,
    pub part_url: String,
    pub bytes: Vec<u8>,
    pub direct_play_confirmed: bool,
    pub content_type: Option<String>,
    pub sampling_rate_hz: Option<u32>,
    pub bit_depth: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexPinStartResult {
    pub pin_id: u64,
    pub code: String,
    pub auth_url: String,
    pub expires_in: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexPinCheckResult {
    pub authorized: bool,
    pub expired: bool,
    pub auth_token: Option<String>,
    pub expires_in: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexCachedAlbum {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artwork_path: Option<String>,
    pub track_count: u32,
    pub total_duration_secs: u64,
    pub format: String,
    pub bit_depth: Option<u32>,
    pub sample_rate: u32,
    pub source: String,
    pub likely_single_file_album: bool,
    pub year: Option<u32>,
    pub genre: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexCachedTrack {
    pub id: u64,
    pub rating_key: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: u64,
    pub format: String,
    pub bit_depth: Option<u32>,
    pub sample_rate: u32,
    pub artwork_path: Option<String>,
    pub collection_artwork_path: Option<String>,
    pub source: String,
    pub album_key: String,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub year: Option<u32>,
    /// The Plex album (`parentRatingKey`) this track belongs to — used to split
    /// distinct same-title albums into separate versions in the album view.
    pub parent_rating_key: Option<String>,
}

/// An artist aggregated client-side from the flat `plex_cache_tracks` table
/// (there is no `plex_cache_artists` table). Mirrors `PlexCachedAlbum`'s
/// aggregation shape; `artwork_path` is a representative track/album thumb
/// (`/library/...`) used as a portrait fallback by the Local Library artists
/// rail when no custom/Qobuz portrait exists.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexCachedArtist {
    pub name: String,
    pub album_count: u32,
    pub track_count: u32,
    pub artwork_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexTrackQualityUpdate {
    pub rating_key: String,
    pub container: Option<String>,
    pub sampling_rate_hz: Option<u32>,
    pub bit_depth: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlexPinResponse {
    id: u64,
    code: String,
    #[serde(default)]
    auth_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Debug, Default)]
struct TrackBuilder {
    rating_key: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    duration_ms: Option<u64>,
    artwork_path: Option<String>,
    collection_artwork_path: Option<String>,
    part_key: Option<String>,
    container: Option<String>,
    codec: Option<String>,
    channels: Option<u32>,
    bitrate_kbps: Option<u32>,
    sampling_rate_hz: Option<u32>,
    bit_depth: Option<u32>,
    track_number: Option<u32>,
    disc_number: Option<u32>,
    year: Option<u32>,
    genres: Vec<String>,
    genre: Option<String>,
    parent_rating_key: Option<String>,
    recording_mbid: Option<String>,
}

fn normalize_base_url(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_string()
}

fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn plex_genres_json(track: &PlexTrack) -> String {
    let mut genres = Vec::<String>::new();
    for value in track.genres.iter().chain(track.genre.iter()) {
        let value = value.trim();
        if !value.is_empty()
            && !genres
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(value))
        {
            genres.push(value.to_string());
        }
    }
    serde_json::to_string(&genres).unwrap_or_else(|_| "[]".to_string())
}

fn plex_genres_from_json(raw: Option<&str>, primary: Option<&str>) -> Vec<String> {
    let mut genres = raw
        .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .unwrap_or_default();
    if genres.is_empty() {
        if let Some(value) = primary.filter(|value| !value.trim().is_empty()) {
            genres.push(value.trim().to_string());
        }
    }
    genres
}

fn open_plex_cache_db() -> Result<Connection, String> {
    let data_dir = dirs::data_dir()
        .ok_or("Could not determine data directory")?
        .join("qbz");
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("Failed to create Plex cache dir: {}", e))?;

    let db_path = data_dir.join("plex_cache.db");
    let conn = Connection::open(db_path)
        .map_err(|e| format!("Failed to open Plex cache database: {}", e))?;

    conn.busy_timeout(Duration::from_millis(2_500))
        .map_err(|e| format!("Failed to set Plex cache busy timeout: {}", e))?;

    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .map_err(|e| format!("Failed to enable WAL for Plex cache database: {}", e))?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS plex_cache_sections (
            section_key TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            server_id TEXT,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS plex_cache_tracks (
            rating_key TEXT PRIMARY KEY,
            section_key TEXT NOT NULL,
            server_id TEXT,
            title TEXT NOT NULL,
            artist TEXT,
            album TEXT,
            duration_ms INTEGER,
            artwork_path TEXT,
            collection_artwork_path TEXT,
            part_key TEXT,
            container TEXT,
            codec TEXT,
            channels INTEGER,
            bitrate_kbps INTEGER,
            sampling_rate_hz INTEGER,
            bit_depth INTEGER,
            genres_json TEXT NOT NULL DEFAULT '[]',
            updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_plex_cache_tracks_section ON plex_cache_tracks(section_key);

        CREATE TABLE IF NOT EXISTS plex_cache_section_sync (
            section_key TEXT PRIMARY KEY,
            server_id TEXT,
            generation INTEGER NOT NULL DEFAULT 0,
            next_start INTEGER NOT NULL DEFAULT 0,
            total_size INTEGER,
            observed_rows INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'idle',
            updated_at INTEGER NOT NULL DEFAULT 0
        );

        -- `/library/sections/<id>/all?type=10` does not always put
        -- `parentThumb` on Track rows, even when the album has a cover. Keep
        -- the authoritative album-metadata answer separately: it is scoped by
        -- server because Plex rating keys are only server-local identities.
        CREATE TABLE IF NOT EXISTS plex_cache_album_artwork (
            server_id TEXT NOT NULL,
            parent_rating_key TEXT NOT NULL,
            artwork_path TEXT,
            checked_at INTEGER NOT NULL,
            PRIMARY KEY(server_id, parent_rating_key)
        );
        ",
    )
    .map_err(|e| format!("Failed to initialize Plex cache schema: {}", e))?;

    // Migration: add track_number, disc_number, album_key, year, genre columns
    // if missing. Year + genre added 2026-04-19 to surface release year and
    // genre for Plex albums inside Local Library and Discography Builder.
    for col in &[
        "track_number INTEGER",
        "disc_number INTEGER",
        "album_key TEXT",
        "year INTEGER",
        "genre TEXT",
        "genres_json TEXT NOT NULL DEFAULT '[]'",
        "collection_artwork_path TEXT",
        // parent_rating_key (2026-06-08): the Plex album a track belongs to, so
        // two same-title albums no longer interleave in the album view. NULL on
        // pre-migration rows (and rows synced before this column) — a re-sync
        // backfills it; the album view falls back to one version meanwhile.
        "parent_rating_key TEXT",
        // Generation 0 is the legacy snapshot. The first completed paged sync
        // promotes observed rows to generation 1 and only then prunes 0.
        "sync_generation INTEGER NOT NULL DEFAULT 0",
        // Identity (2026-08-28): the MusicBrainz recording id from the item's
        // Guid. NULL until the next sync re-reads the section with Guids.
        "recording_mbid TEXT",
    ] {
        let stmt = format!("ALTER TABLE plex_cache_tracks ADD COLUMN {col}");
        let _ = conn.execute(&stmt, []);
    }

    // Backfill album_key for existing rows that have NULL
    {
        let needs_backfill: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM plex_cache_tracks WHERE album_key IS NULL LIMIT 1)",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if needs_backfill {
            let mut read_stmt = conn
                .prepare("SELECT rating_key, artist, album FROM plex_cache_tracks WHERE album_key IS NULL")
                .map_err(|e| format!("Failed to prepare album_key backfill read: {}", e))?;
            let rows: Vec<(String, String, String)> = read_stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?
                            .unwrap_or_else(|| "Unknown Artist".to_string()),
                        row.get::<_, Option<String>>(2)?
                            .unwrap_or_else(|| "Unknown Album".to_string()),
                    ))
                })
                .map_err(|e| format!("Failed to read tracks for album_key backfill: {}", e))?
                .filter_map(|r| r.ok())
                .collect();
            drop(read_stmt);

            for (rating_key, artist_raw, album_raw) in &rows {
                let artist = decode_xml_entities(artist_raw.trim());
                let artist = if artist.is_empty() {
                    "Unknown Artist".to_string()
                } else {
                    artist
                };
                let album = decode_xml_entities(album_raw.trim());
                let album = if album.is_empty() {
                    "Unknown Album".to_string()
                } else {
                    album
                };
                let album = normalize_album_title(Some(&artist), &album);
                let key = plex_album_key(&artist, &album);
                let _ = conn.execute(
                    "UPDATE plex_cache_tracks SET album_key = ?1 WHERE rating_key = ?2",
                    params![key, rating_key],
                );
            }
        }
    }

    Ok(conn)
}

/// The default-timeout Plex client, built ONCE.
///
/// It used to be rebuilt on every call, and a fresh `reqwest::Client` is not
/// free: new connection pool (so no keep-alive to reuse), fresh TLS handshake,
/// and a fresh DNS resolution. On Plex that last one is the expensive part —
/// LAN servers are addressed as `https://<ip>.<hash>.plex.direct:32400`, and
/// `*.plex.direct` resolves through Plex's own nameservers, i.e. over the
/// INTERNET. So every play paid an internet DNS round trip plus a TLS handshake
/// before the first byte of a file that was sitting on the local network.
///
/// That is the shape the owner reported (2026-08-13): playing a Plex track took
/// LONGER than streaming a Qobuz track from the internet, which no amount of
/// LAN transfer time can explain — but a per-call client can, because the Qobuz
/// path reuses one long-lived client and this one threw its away every time.
///
/// Cached here rather than at the call sites so every Plex entry point gets it.
/// `build_plex_client_with_timeout` stays uncached: its callers pick a short
/// bound on purpose (quality hydration), and those are genuinely different
/// clients.
static PLEX_CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
    std::sync::OnceLock::new();

fn build_plex_client() -> Result<reqwest::Client, String> {
    PLEX_CLIENT
        .get_or_init(|| build_plex_client_with_timeout(Duration::from_secs(120)))
        .clone()
}

/// Non-secret client identity Plex expects on media-part requests.
///
/// Metadata and full-body playback already use [`build_plex_client`], which
/// installs this profile as default headers. Progressive playback resolves the
/// URL here but performs the byte transfer in the shared player transport, so
/// it must carry the same profile with the resolved location. The token stays
/// in the redacted query URL; none of these values is a credential.
pub fn plex_media_request_headers() -> Vec<(String, String)> {
    vec![
        ("X-Plex-Product".into(), "QBZ".into()),
        ("X-Plex-Version".into(), "0.1-poc".into()),
        ("X-Plex-Device".into(), "QBZ Desktop".into()),
        ("X-Plex-Platform".into(), std::env::consts::OS.into()),
        ("X-Plex-Client-Identifier".into(), "qbz-plex-lan-poc".into()),
    ]
}

/// Build the LAN Plex client with a caller-chosen request timeout. Quality
/// hydration uses a short (~5s) bound per metadata call so a single dead/slow
/// `rating_key` cannot stall the batch (matches the Svelte 5000 ms per-call
/// timeout); normal browsing keeps the generous 120s default.
fn build_plex_client_with_timeout(timeout: Duration) -> Result<reqwest::Client, String> {
    let mut headers = HeaderMap::new();
    headers.insert("X-Plex-Product", HeaderValue::from_static("QBZ"));
    headers.insert("X-Plex-Version", HeaderValue::from_static("0.1-poc"));
    headers.insert("X-Plex-Device", HeaderValue::from_static("QBZ Desktop"));
    headers.insert(
        "X-Plex-Platform",
        HeaderValue::from_static(std::env::consts::OS),
    );
    headers.insert(
        "X-Plex-Client-Identifier",
        HeaderValue::from_static("qbz-plex-lan-poc"),
    );

    qbz_net_proxy::apply_current(reqwest::Client::builder())
        .map_err(|e| format!("Failed to apply proxy configuration: {}", e))?
        .default_headers(headers)
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| format!("Failed to create Plex HTTP client: {}", e))
}

fn build_plex_auth_client(client_identifier: &str) -> Result<reqwest::Client, String> {
    let mut headers = HeaderMap::new();
    headers.insert("X-Plex-Product", HeaderValue::from_static("QBZ"));
    headers.insert("X-Plex-Version", HeaderValue::from_static("0.1-poc"));
    headers.insert("X-Plex-Device", HeaderValue::from_static("QBZ Desktop"));
    headers.insert(
        "X-Plex-Platform",
        HeaderValue::from_static(std::env::consts::OS),
    );
    headers.insert(
        "X-Plex-Client-Identifier",
        HeaderValue::from_str(client_identifier)
            .map_err(|e| format!("Invalid Plex client identifier: {}", e))?,
    );
    headers.insert("Accept", HeaderValue::from_static("application/json"));

    qbz_net_proxy::apply_current(reqwest::Client::builder())
        .map_err(|e| format!("Failed to apply proxy configuration: {}", e))?
        .default_headers(headers)
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| format!("Failed to create Plex auth HTTP client: {}", e))
}

fn build_plex_auth_url() -> String {
    "https://plex.tv/link".to_string()
}

fn with_token(url: &str, token: &str) -> String {
    let sep = if url.contains('?') { "&" } else { "?" };
    format!("{url}{sep}X-Plex-Token={token}")
}

/// Ask Plex for the original media bytes rather than its bare part route.
///
/// Some Plex servers advertise a valid `/library/parts/.../file` key but
/// answer HTTP 500 when that key is fetched without `download=1`; the same
/// part immediately serves ranged `audio/flac` bytes with the flag. Plex Web
/// and Plexamp hide that server quirk behind their delivery negotiation. QBZ
/// needs the explicit flag because it Range-streams the original part itself.
fn as_download(url: &str) -> String {
    if url.split_once('?').is_some_and(|(_, query)| {
        query.split('&').any(|pair| {
            pair.split_once('=')
                .is_some_and(|(key, value)| key.eq_ignore_ascii_case("download") && value == "1")
        })
    }) {
        return url.to_string();
    }
    let sep = if url.contains('?') { "&" } else { "?" };
    format!("{url}{sep}download=1")
}

/// Render a request failure without reqwest's URL. Plex authentication is
/// carried in the query string, and reqwest's Display output includes that URL
/// for several error kinds. Callers may surface these strings in UI logs, so
/// only retain a coarse reason and the non-sensitive numeric status.
fn safe_http_error(context: &str, error: &reqwest::Error) -> String {
    let reason = if error.is_timeout() {
        "request timed out".to_string()
    } else if let Some(status) = error.status() {
        format!("server returned HTTP {}", status.as_u16())
    } else if error.is_connect() {
        "connection failed".to_string()
    } else if error.is_decode() {
        "response decoding failed".to_string()
    } else if error.is_body() {
        "response body failed".to_string()
    } else {
        "request failed".to_string()
    };
    format!("{context}: {reason}")
}

fn parse_u64(v: Option<String>) -> Option<u64> {
    v.and_then(|s| s.parse::<u64>().ok())
}

fn parse_u32(v: Option<String>) -> Option<u32> {
    v.and_then(|s| s.parse::<u32>().ok())
}

fn decode_xml_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'&' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b';' && (j - i) <= 12 {
                j += 1;
            }

            if j < bytes.len() && bytes[j] == b';' {
                let entity = &input[i + 1..j];
                let decoded = match entity {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                        u32::from_str_radix(&entity[2..], 16)
                            .ok()
                            .and_then(char::from_u32)
                    }
                    _ if entity.starts_with('#') => {
                        entity[1..].parse::<u32>().ok().and_then(char::from_u32)
                    }
                    _ => None,
                };

                if let Some(ch) = decoded {
                    out.push(ch);
                    i = j + 1;
                    continue;
                }
            }
        }

        if let Some(ch) = input[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }

    out
}

fn normalize_album_title(artist: Option<&str>, album: &str) -> String {
    let trimmed_album = album.trim();
    let Some(artist_value) = artist.map(str::trim).filter(|a| !a.is_empty()) else {
        return trimmed_album.to_string();
    };

    for sep in [" - ", " — ", " – ", ": "] {
        let prefix = format!("{artist_value}{sep}");
        if trimmed_album.starts_with(&prefix) {
            return trimmed_album[prefix.len()..].trim().to_string();
        }
    }

    trimmed_album.to_string()
}

fn get_attr(tag: &str, key: &str) -> Option<String> {
    let tag_bytes = tag.as_bytes();
    let key_bytes = key.as_bytes();
    if key_bytes.is_empty() || tag_bytes.len() < key_bytes.len() + 2 {
        return None;
    }

    let mut i = 0usize;
    while i + key_bytes.len() + 2 <= tag_bytes.len() {
        if &tag_bytes[i..i + key_bytes.len()] != key_bytes {
            i += 1;
            continue;
        }

        let prev = if i == 0 { b' ' } else { tag_bytes[i - 1] };
        if !prev.is_ascii_whitespace() && prev != b'<' {
            i += 1;
            continue;
        }

        let eq_idx = i + key_bytes.len();
        if eq_idx >= tag_bytes.len() || tag_bytes[eq_idx] != b'=' {
            i += 1;
            continue;
        }
        let quote_idx = eq_idx + 1;
        if quote_idx >= tag_bytes.len() || tag_bytes[quote_idx] != b'"' {
            i += 1;
            continue;
        }

        let value_start = quote_idx + 1;
        let value_rel_end = tag[value_start..].find('"')?;
        let value_end = value_start + value_rel_end;
        return Some(decode_xml_entities(tag[value_start..value_end].trim()));
    }

    None
}

fn find_first_tag(xml: &str, tag_name: &str) -> Option<String> {
    let needle = format!("<{tag_name}");
    let start = xml.find(&needle)?;
    let rest = &xml[start..];
    let end = rest.find('>')?;
    Some(rest[..=end].to_string())
}

fn collect_start_tags(xml: &str, tag_name: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let open = format!("<{tag_name}");
    let mut offset = 0usize;

    while let Some(pos) = xml[offset..].find(&open) {
        let start = offset + pos;
        let rest = &xml[start..];
        let Some(end) = rest.find('>') else {
            break;
        };
        tags.push(rest[..=end].to_string());
        offset = start + end + 1;
    }

    tags
}

fn collect_tag_blocks(xml: &str, tag_name: &str) -> Vec<(String, String)> {
    let mut blocks = Vec::new();
    let open = format!("<{tag_name}");
    let close = format!("</{tag_name}>");
    let mut offset = 0usize;

    while let Some(pos) = xml[offset..].find(&open) {
        let start = offset + pos;
        let rest = &xml[start..];
        let Some(open_end_rel) = rest.find('>') else {
            break;
        };
        let open_end = start + open_end_rel;
        let start_tag = &xml[start..=open_end];
        let is_self_closing = start_tag.trim_end().ends_with("/>");

        if is_self_closing {
            blocks.push((start_tag.to_string(), String::new()));
            offset = open_end + 1;
            continue;
        }

        let after_open = open_end + 1;
        let Some(close_rel) = xml[after_open..].find(&close) else {
            break;
        };
        let close_start = after_open + close_rel;
        let inner = &xml[after_open..close_start];
        blocks.push((start_tag.to_string(), inner.to_string()));
        offset = close_start + close.len();
    }

    blocks
}

fn parse_server_info(xml: &str) -> PlexServerInfo {
    let tag = find_first_tag(xml, "MediaContainer");
    PlexServerInfo {
        friendly_name: tag.as_ref().and_then(|t| get_attr(t, "friendlyName")),
        version: tag.as_ref().and_then(|t| get_attr(t, "version")),
        machine_identifier: tag.as_ref().and_then(|t| get_attr(t, "machineIdentifier")),
    }
}

fn parse_music_sections(xml: &str) -> Vec<PlexMusicSection> {
    let mut sections = Vec::new();
    for tag in collect_start_tags(xml, "Directory") {
        if get_attr(&tag, "type").as_deref() != Some("artist") {
            continue;
        }
        if let (Some(key), Some(title)) = (get_attr(&tag, "key"), get_attr(&tag, "title")) {
            sections.push(PlexMusicSection { key, title });
        }
    }
    sections
}

/// The track listing is not an artwork authority: Plex may omit both `thumb`
/// and `parentThumb` there while `/library/metadata/<parentRatingKey>` still
/// exposes the album cover (and that cover can point at a *different* rating
/// key). Read the returned token instead of synthesizing a URL from the album
/// id; the latter is a real 404 for inherited/default Plex artwork.
fn parse_album_artwork_path(xml: &str) -> Option<String> {
    let directory_thumb = collect_start_tags(xml, "Directory")
        .into_iter()
        .find(|tag| get_attr(tag, "type").as_deref() == Some("album"))
        .and_then(|tag| get_attr(&tag, "thumb"));
    directory_thumb
        .or_else(|| {
            collect_start_tags(xml, "Image")
                .into_iter()
                .find(|tag| get_attr(tag, "type").as_deref() == Some("coverPoster"))
                .and_then(|tag| get_attr(&tag, "url"))
        })
        .map(|path| path.trim().to_string())
        .filter(|path| {
            !path.is_empty() && (path.starts_with("/library/") || path.starts_with("/photo/"))
        })
}

fn parse_track_block(start_tag: &str, inner_xml: &str) -> Option<PlexTrack> {
    // Plex exposes album release year via parentYear on the Track element
    // (same structure that drives the Plex client's year display); fall back
    // to year, and to the first 4 digits of originallyAvailableAt / parentOriginallyAvailableAt
    // (ISO date) when parentYear is missing on older metadata writes.
    let year = parse_u32(get_attr(start_tag, "parentYear"))
        .or_else(|| parse_u32(get_attr(start_tag, "year")))
        .or_else(|| {
            let iso = get_attr(start_tag, "parentOriginallyAvailableAt")
                .or_else(|| get_attr(start_tag, "originallyAvailableAt"));
            parse_u32(iso.and_then(|s| s.get(..4).map(|s| s.to_string())))
        });

    // Genre may appear either as an attribute or as several nested
    // `<Genre tag="..."/>` rows. Preserve all of them; the old singular field
    // remains the first value for compatibility.
    let mut genres = Vec::<String>::new();
    if let Some(value) = get_attr(start_tag, "genre") {
        if !value.trim().is_empty() {
            genres.push(value.trim().to_string());
        }
    }
    for tag in collect_start_tags(inner_xml, "Genre") {
        if let Some(value) = get_attr(&tag, "tag").filter(|value| !value.trim().is_empty()) {
            let value = value.trim().to_string();
            if !genres
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&value))
            {
                genres.push(value);
            }
        }
    }
    let genre = genres.first().cloned();

    let mut t = TrackBuilder {
        rating_key: get_attr(start_tag, "ratingKey"),
        title: get_attr(start_tag, "title"),
        artist: get_attr(start_tag, "grandparentTitle")
            .or_else(|| get_attr(start_tag, "originalTitle")),
        album: get_attr(start_tag, "parentTitle"),
        duration_ms: parse_u64(get_attr(start_tag, "duration")),
        // Preserve the specificity hierarchy instead of flattening it here.
        // A track/disc thumb may differ from the parent album cover; callers
        // choose item -> collection -> placeholder for playback, while album
        // aggregation chooses collection -> item.
        artwork_path: get_attr(start_tag, "thumb"),
        collection_artwork_path: get_attr(start_tag, "parentThumb"),
        track_number: parse_u32(get_attr(start_tag, "index")),
        disc_number: parse_u32(get_attr(start_tag, "parentIndex")),
        year,
        genres,
        genre,
        parent_rating_key: get_attr(start_tag, "parentRatingKey"),
        ..Default::default()
    };

    if let Some(title) = t.title.as_mut() {
        *title = title.trim().to_string();
    }
    if let Some(artist) = t.artist.as_mut() {
        *artist = artist.trim().to_string();
    }
    if let Some(album) = t.album.as_mut() {
        let normalized = normalize_album_title(t.artist.as_deref(), album);
        *album = normalized;
    }

    for media_tag in collect_start_tags(inner_xml, "Media") {
        t.container = get_attr(&media_tag, "container");
        t.bitrate_kbps = parse_u32(get_attr(&media_tag, "bitrate"));
        t.sampling_rate_hz = parse_u32(get_attr(&media_tag, "samplingRate"));
        t.bit_depth = parse_u32(get_attr(&media_tag, "bitDepth"));
        break;
    }

    // Stream metadata is more accurate for audio details than Media-level fields.
    // Prefer selected audio stream when available, otherwise use the first audio stream.
    let mut selected_audio_stream: Option<String> = None;
    let mut first_audio_stream: Option<String> = None;
    for stream_tag in collect_start_tags(inner_xml, "Stream") {
        let is_audio = get_attr(&stream_tag, "streamType").as_deref() == Some("2")
            || get_attr(&stream_tag, "codecType").as_deref() == Some("audio");
        if !is_audio {
            continue;
        }
        if first_audio_stream.is_none() {
            first_audio_stream = Some(stream_tag.clone());
        }
        if get_attr(&stream_tag, "selected").as_deref() == Some("1") {
            selected_audio_stream = Some(stream_tag);
            break;
        }
    }

    if let Some(stream_tag) = selected_audio_stream.or(first_audio_stream) {
        if let Some(codec) = get_attr(&stream_tag, "codec") {
            t.codec = Some(codec);
        }
        if let Some(channels) = parse_u32(get_attr(&stream_tag, "channels")) {
            t.channels = Some(channels);
        }
        if let Some(bitrate) = parse_u32(get_attr(&stream_tag, "bitrate")) {
            t.bitrate_kbps = Some(bitrate);
        }
        if let Some(rate) = parse_u32(get_attr(&stream_tag, "samplingRate")) {
            t.sampling_rate_hz = Some(rate);
        }
        if let Some(depth) = parse_u32(get_attr(&stream_tag, "bitDepth")) {
            t.bit_depth = Some(depth);
        }
    }

    for part_tag in collect_start_tags(inner_xml, "Part") {
        t.part_key = get_attr(&part_tag, "key");
        if t.part_key.is_some() {
            break;
        }
    }

    // `<Guid id="mbid://<uuid>"/>` — present only with includeGuids=1 and
    // only for tracks the Plex Music agent matched. Other schemes (plex://)
    // are not identities anyone else can join on, so they are ignored.
    for guid_tag in collect_start_tags(inner_xml, "Guid") {
        if let Some(mbid) = get_attr(&guid_tag, "id")
            .as_deref()
            .and_then(|id| id.strip_prefix("mbid://"))
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            t.recording_mbid = Some(mbid.to_string());
            break;
        }
    }

    let (Some(rating_key), Some(title)) = (t.rating_key, t.title) else {
        return None;
    };

    Some(PlexTrack {
        rating_key,
        title,
        artist: t.artist,
        album: t.album,
        duration_ms: t.duration_ms,
        artwork_path: t.artwork_path,
        collection_artwork_path: t.collection_artwork_path,
        part_key: t.part_key,
        container: t.container,
        codec: t.codec,
        channels: t.channels,
        bitrate_kbps: t.bitrate_kbps,
        sampling_rate_hz: t.sampling_rate_hz,
        bit_depth: t.bit_depth,
        track_number: t.track_number,
        disc_number: t.disc_number,
        year: t.year,
        genres: t.genres,
        genre: t.genre,
        parent_rating_key: t.parent_rating_key,
        recording_mbid: t.recording_mbid,
    })
}

fn parse_tracks(xml: &str, limit: Option<u32>) -> Vec<PlexTrack> {
    let mut tracks = Vec::new();

    for (start_tag, inner_xml) in collect_tag_blocks(xml, "Track") {
        if let Some(track) = parse_track_block(&start_tag, &inner_xml) {
            tracks.push(track);
            if let Some(max) = limit {
                if tracks.len() >= max as usize {
                    break;
                }
            }
        }
    }

    tracks
}

fn parse_track_page(
    xml: &str,
    requested_start: u64,
    requested_size: u64,
) -> Result<PlexTrackPage, String> {
    let container = find_first_tag(xml, "MediaContainer")
        .ok_or_else(|| "Plex track page has no MediaContainer".to_string())?;
    let offset = parse_u64(get_attr(&container, "offset")).unwrap_or(requested_start);
    let response_size = parse_u64(get_attr(&container, "size"))
        .ok_or_else(|| "Plex track page has no size".to_string())?;
    let total_size = parse_u64(get_attr(&container, "totalSize"))
        .ok_or_else(|| "Plex track page has no totalSize".to_string())?;
    if offset != requested_start {
        return Err(format!(
            "Plex track page offset mismatch: requested {requested_start}, received {offset}"
        ));
    }
    if response_size > requested_size {
        return Err(format!(
            "Plex track page exceeded requested size: requested {requested_size}, received {response_size}"
        ));
    }

    let blocks = collect_tag_blocks(xml, "Track");
    if blocks.len() as u64 != response_size {
        return Err(format!(
            "Plex track page size mismatch: container {response_size}, XML {}",
            blocks.len()
        ));
    }
    let mut tracks = Vec::with_capacity(blocks.len());
    let mut ids = HashSet::with_capacity(blocks.len());
    for (start_tag, inner_xml) in blocks {
        let track = parse_track_block(&start_tag, &inner_xml)
            .ok_or_else(|| "Plex track page contains an incomplete Track".to_string())?;
        if !ids.insert(track.rating_key.clone()) {
            return Err("Plex track page contains a duplicate ratingKey".to_string());
        }
        tracks.push(track);
    }
    let next_start = offset.saturating_add(response_size);
    if next_start > total_size || (response_size == 0 && offset < total_size) {
        return Err(format!(
            "Plex track page cannot advance safely: offset {offset}, size {response_size}, total {total_size}"
        ));
    }
    Ok(PlexTrackPage {
        tracks,
        offset,
        response_size,
        total_size,
    })
}

fn synthetic_track_id(rating_key: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    rating_key.hash(&mut hasher);
    hasher.finish()
}

fn playback_track_id(rating_key: &str) -> u64 {
    rating_key
        .parse::<u64>()
        .unwrap_or_else(|_| synthetic_track_id(rating_key))
}

/// Content-hash album key for a Plex album (`plex:<hash(artist::album)>`). This
/// is the stable per-album identity the grid card carries and that
/// `plex_cache_get_album_tracks` queries by — distinct from the per-edition
/// `parent_rating_key`. Public so the frontend can recover it for a played
/// track (whose `album_group_key` is the per-edition split key, not this).
pub fn plex_album_key(artist: &str, album: &str) -> String {
    let mut hasher = DefaultHasher::new();
    artist.hash(&mut hasher);
    "::".hash(&mut hasher);
    album.hash(&mut hasher);
    format!("plex:{}", hasher.finish())
}

fn is_direct_part_key(part_key: &str) -> bool {
    part_key.starts_with("/library/parts/") && part_key.contains("/file")
}

pub async fn plex_ping(base_url: String, token: String) -> Result<PlexServerInfo, String> {
    let client = build_plex_client()?;
    let base = normalize_base_url(&base_url);
    let url = with_token(&format!("{base}/"), &token);

    let xml = client
        .get(url)
        .send()
        .await
        .map_err(|e| safe_http_error("Plex ping", &e))?
        .error_for_status()
        .map_err(|e| safe_http_error("Plex ping", &e))?
        .text()
        .await
        .map_err(|e| safe_http_error("Failed to read Plex ping response", &e))?;

    Ok(parse_server_info(&xml))
}

pub async fn plex_get_music_sections(
    base_url: String,
    token: String,
) -> Result<Vec<PlexMusicSection>, String> {
    let client = build_plex_client()?;
    let base = normalize_base_url(&base_url);
    let url = with_token(&format!("{base}/library/sections"), &token);

    let xml = client
        .get(url)
        .send()
        .await
        .map_err(|e| safe_http_error("Plex sections request", &e))?
        .error_for_status()
        .map_err(|e| safe_http_error("Plex sections request", &e))?
        .text()
        .await
        .map_err(|e| safe_http_error("Failed to read Plex sections response", &e))?;

    Ok(parse_music_sections(&xml))
}

pub async fn plex_get_section_tracks(
    base_url: String,
    token: String,
    section_key: String,
    limit: Option<u32>,
) -> Result<Vec<PlexTrack>, String> {
    let effective_limit = limit.filter(|v| *v > 0);
    let mut tracks = Vec::new();
    let mut start = 0_u64;
    loop {
        let remaining = effective_limit
            .map(|limit| u64::from(limit).saturating_sub(tracks.len() as u64))
            .unwrap_or(DEFAULT_PLEX_TRACK_PAGE_SIZE);
        if remaining == 0 {
            break;
        }
        let page_size = remaining.min(DEFAULT_PLEX_TRACK_PAGE_SIZE);
        let page = plex_get_section_tracks_page(
            base_url.clone(),
            token.clone(),
            section_key.clone(),
            start,
            page_size,
        )
        .await?;
        start = page.next_start();
        let has_more = page.has_more();
        tracks.extend(page.tracks);
        if !has_more {
            break;
        }
    }
    Ok(tracks)
}

pub async fn plex_get_section_tracks_page(
    base_url: String,
    token: String,
    section_key: String,
    start: u64,
    page_size: u64,
) -> Result<PlexTrackPage, String> {
    if page_size == 0 {
        return Err("Plex track page size must be positive".to_string());
    }
    let client = build_plex_client()?;
    let base = normalize_base_url(&base_url);
    // includeGuids=1: the item's external ids (`<Guid id="mbid://…"/>`)
    // travel with the row; without it Plex omits them from listings.
    let list_url = format!("{base}/library/sections/{section_key}/all?type=10&includeGuids=1");
    let url = with_token(&list_url, &token);
    let mut response = client
        .get(url)
        .header("X-Plex-Container-Start", start.to_string())
        .header("X-Plex-Container-Size", page_size.to_string())
        .send()
        .await
        .map_err(|e| safe_http_error("Plex tracks page request", &e))?
        .error_for_status()
        .map_err(|e| safe_http_error("Plex tracks page request", &e))?;
    if response
        .content_length()
        .is_some_and(|bytes| bytes > MAX_PLEX_TRACK_PAGE_BYTES as u64)
    {
        return Err("Plex tracks page exceeds the bounded response size".to_string());
    }
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or(0)
            .min(MAX_PLEX_TRACK_PAGE_BYTES as u64) as usize,
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| safe_http_error("Failed to read Plex tracks page", &e))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_PLEX_TRACK_PAGE_BYTES {
            return Err("Plex tracks page exceeds the bounded response size".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    let xml = String::from_utf8(body)
        .map_err(|_| "Plex tracks page is not valid UTF-8 XML".to_string())?;
    parse_track_page(&xml, start, page_size)
}

/// Fetch the album-level cover Plex omitted from its flat track listing.
/// `parent_rating_key` is a path segment supplied by the same Plex server; it
/// is still validated before interpolation so malformed cache data cannot
/// escape the metadata endpoint.
pub async fn plex_get_album_artwork_path(
    base_url: String,
    token: String,
    parent_rating_key: String,
) -> Result<Option<String>, String> {
    let key = parent_rating_key.trim();
    if key.is_empty() || key.chars().any(|ch| matches!(ch, '/' | '?' | '#')) {
        return Err("Invalid Plex album rating key".to_string());
    }
    let client = build_plex_client()?;
    let base = normalize_base_url(&base_url);
    let url = with_token(&format!("{base}/library/metadata/{key}"), &token);
    let xml = client
        .get(url)
        .send()
        .await
        .map_err(|e| safe_http_error("Plex album metadata request", &e))?
        .error_for_status()
        .map_err(|e| safe_http_error("Plex album metadata request", &e))?
        .text()
        .await
        .map_err(|e| safe_http_error("Failed to read Plex album metadata response", &e))?;
    Ok(parse_album_artwork_path(&xml))
}

pub async fn plex_get_track_metadata(
    base_url: String,
    token: String,
    rating_key: String,
) -> Result<PlexTrack, String> {
    let client = build_plex_client()?;
    plex_get_track_metadata_with_client(&client, &base_url, &token, &rating_key).await
}

/// Shared metadata fetch that reuses an existing client. The hydration path
/// passes a short-timeout client so it bounds each per-track call without
/// rebuilding a client per `rating_key`.
async fn plex_get_track_metadata_with_client(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    rating_key: &str,
) -> Result<PlexTrack, String> {
    let base = normalize_base_url(base_url);
    let detail_url = format!("{base}/library/metadata/{rating_key}");
    let url = with_token(&detail_url, token);

    let xml = client
        .get(url)
        .send()
        .await
        .map_err(|e| safe_http_error("Plex track metadata request", &e))?
        .error_for_status()
        .map_err(|e| safe_http_error("Plex track metadata request", &e))?
        .text()
        .await
        .map_err(|e| safe_http_error("Failed to read Plex track metadata response", &e))?;

    parse_tracks(&xml, Some(1))
        .into_iter()
        .next()
        .ok_or_else(|| "Plex track metadata not found".to_string())
}

/// Hydrate real per-track quality for a set of Plex `rating_keys`.
///
/// Frontend-agnostic orchestration over the two existing primitives:
/// `plex_get_track_metadata` (the real per-track `container` / `samplingRate` /
/// `bitDepth` the bulk `/all` list omits) is fetched per key, then
/// `plex_cache_update_track_quality` persists the result (COALESCE write-back,
/// so a NULL field never erases an existing value). The keys come from the
/// DB-NULL queue (`plex_cache_get_tracks_needing_hydration`) — NOT a value
/// heuristic — so a genuine 16/44.1 FLAC is written once and never re-probed.
///
/// Calls run in batches of `BATCH`, sequentially within a batch (matching the
/// Svelte reference), each bounded to ~5s by the client timeout. Failures and
/// timeouts are skipped so one dead key cannot abort the batch. Returns the
/// successfully-fetched updates so the caller can refresh in-memory state
/// without re-reading the cache.
pub async fn plex_hydrate_album_quality(
    base_url: String,
    token: String,
    rating_keys: Vec<String>,
) -> Result<Vec<PlexTrackQualityUpdate>, String> {
    const BATCH: usize = 5;
    if rating_keys.is_empty() {
        return Ok(Vec::new());
    }

    // Short per-call bound (5s) so a slow/dead rating_key can't stall the batch.
    let client = build_plex_client_with_timeout(Duration::from_secs(5))?;
    let mut updates: Vec<PlexTrackQualityUpdate> = Vec::with_capacity(rating_keys.len());

    for chunk in rating_keys.chunks(BATCH) {
        for key in chunk {
            match plex_get_track_metadata_with_client(&client, &base_url, &token, key).await {
                Ok(track) => updates.push(PlexTrackQualityUpdate {
                    rating_key: track.rating_key,
                    container: track.container,
                    sampling_rate_hz: track.sampling_rate_hz,
                    bit_depth: track.bit_depth,
                }),
                Err(_) => continue, // skip dead/410/timeout keys, keep the batch alive
            }
        }
    }

    // Persist (COALESCE write-back). Fire the DB write here so the caller's
    // single await gives both fetched + persisted; persistence MUST complete
    // before the UI refresh (fixes the Svelte fire-and-forget race).
    if !updates.is_empty() {
        plex_cache_update_track_quality(updates.clone())?;
    }

    Ok(updates)
}

pub async fn plex_auth_pin_start(client_identifier: String) -> Result<PlexPinStartResult, String> {
    let client = build_plex_auth_client(&client_identifier)?;
    let pin = client
        .post("https://plex.tv/api/v2/pins?strong=false")
        .send()
        .await
        .map_err(|e| safe_http_error("Plex auth pin request", &e))?
        .error_for_status()
        .map_err(|e| safe_http_error("Plex auth pin request", &e))?
        .json::<PlexPinResponse>()
        .await
        .map_err(|e| safe_http_error("Failed to parse Plex auth pin response", &e))?;

    Ok(PlexPinStartResult {
        pin_id: pin.id,
        code: pin.code.clone(),
        auth_url: build_plex_auth_url(),
        expires_in: pin.expires_in,
    })
}

pub async fn plex_auth_pin_check(
    client_identifier: String,
    pin_id: u64,
    code: Option<String>,
) -> Result<PlexPinCheckResult, String> {
    let client = build_plex_auth_client(&client_identifier)?;
    let base_url = format!("https://plex.tv/api/v2/pins/{}", pin_id);
    let request = if let Some(pin_code) = code {
        client.get(format!("{}?code={}", base_url, pin_code))
    } else {
        client.get(base_url)
    };
    let pin = request
        .send()
        .await
        .map_err(|e| safe_http_error("Plex auth pin check request", &e))?
        .error_for_status()
        .map_err(|e| safe_http_error("Plex auth pin check request", &e))?
        .json::<PlexPinResponse>()
        .await
        .map_err(|e| safe_http_error("Failed to parse Plex auth pin check response", &e))?;

    Ok(PlexPinCheckResult {
        authorized: pin.auth_token.is_some(),
        expired: pin.expires_in == Some(0),
        auth_token: pin.auth_token,
        expires_in: pin.expires_in,
    })
}

pub async fn plex_open_auth_url(url: String) -> Result<(), String> {
    open::that(&url).map_err(|e| format!("Failed to open browser: {}", e))
}

pub fn plex_cache_get_sections() -> Result<Vec<PlexMusicSection>, String> {
    let conn = open_plex_cache_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT section_key, title
             FROM plex_cache_sections
             ORDER BY title COLLATE NOCASE",
        )
        .map_err(|e| format!("Failed to prepare Plex cache sections query: {}", e))?;

    let rows = stmt
        .query_map([], |row| {
            Ok(PlexMusicSection {
                key: row.get(0)?,
                title: row.get(1)?,
            })
        })
        .map_err(|e| format!("Failed to query Plex cache sections: {}", e))?;

    let mut sections = Vec::new();
    for row in rows {
        sections.push(row.map_err(|e| format!("Failed to read Plex cache section row: {}", e))?);
    }
    Ok(sections)
}

pub fn plex_cache_save_sections(
    server_id: Option<String>,
    sections: Vec<PlexMusicSection>,
) -> Result<usize, String> {
    let mut conn = open_plex_cache_db()?;
    let tx = conn
        .transaction()
        .map_err(|e| format!("Failed to start Plex cache sections transaction: {}", e))?;

    let now = now_epoch_secs();
    // A successful `/library/sections` response is the authoritative list.
    // Replace it inside this transaction so a server failure before this call
    // preserves the old list, while removed libraries do not remain as ghosts.
    tx.execute("DELETE FROM plex_cache_sections", [])
        .map_err(|e| format!("Failed to replace Plex cache sections: {}", e))?;
    for section in &sections {
        tx.execute(
            "INSERT INTO plex_cache_sections (section_key, title, server_id, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(section_key) DO UPDATE SET
                title = excluded.title,
                server_id = excluded.server_id,
                updated_at = excluded.updated_at",
            params![section.key, section.title, server_id, now],
        )
        .map_err(|e| format!("Failed to upsert Plex cache section: {}", e))?;
    }

    tx.commit()
        .map_err(|e| format!("Failed to commit Plex cache sections transaction: {}", e))?;
    Ok(sections.len())
}

/// Collection artwork as seen by every cache reader. Direct `parentThumb`
/// remains authoritative; the metadata table only fills the hole left by the
/// flat track endpoint. Keep this expression centralized so Albums, Tracks,
/// playlists and the source registry cannot disagree about the same row.
const EFFECTIVE_COLLECTION_ARTWORK_SQL: &str =
    "COALESCE(NULLIF(plex_cache_tracks.collection_artwork_path,''),
       (SELECT NULLIF(album_art.artwork_path,'')
          FROM plex_cache_album_artwork album_art
         WHERE album_art.server_id =
               COALESCE(NULLIF(plex_cache_tracks.server_id,''),'default')
           AND album_art.parent_rating_key = plex_cache_tracks.parent_rating_key))";

fn album_artwork_candidates_in(
    conn: &Connection,
    server_id: &str,
    stale_before: i64,
) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT tracks.parent_rating_key
               FROM plex_cache_tracks tracks
               LEFT JOIN plex_cache_album_artwork album_art
                 ON album_art.server_id = COALESCE(NULLIF(tracks.server_id,''),'default')
                AND album_art.parent_rating_key = tracks.parent_rating_key
              WHERE COALESCE(NULLIF(tracks.server_id,''),'default') = ?1
                AND COALESCE(tracks.parent_rating_key,'') != ''
                AND COALESCE(tracks.artwork_path,'') = ''
                AND COALESCE(tracks.collection_artwork_path,'') = ''
                AND (album_art.parent_rating_key IS NULL OR album_art.checked_at <= ?2)
              ORDER BY tracks.parent_rating_key",
        )
        .map_err(|e| format!("Failed to prepare Plex album artwork candidates: {e}"))?;
    let rows = stmt
        .query_map(params![server_id, stale_before], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|e| format!("Failed to query Plex album artwork candidates: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("Failed to read Plex album artwork candidate: {e}"))
}

/// Album ids whose track rows omit both item and parent art, excluding recent
/// positive *and* negative metadata checks.
pub fn plex_cache_album_artwork_candidates(
    server_id: Option<String>,
) -> Result<Vec<String>, String> {
    let conn = open_plex_cache_db()?;
    let server_id = server_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default");
    let stale_before = now_epoch_secs()
        .saturating_sub(i64::try_from(PLEX_ALBUM_ARTWORK_MAX_AGE_SECS).unwrap_or(i64::MAX));
    album_artwork_candidates_in(&conn, server_id, stale_before)
}

fn save_album_artwork_in(
    conn: &mut Connection,
    server_id: &str,
    rows: &[(String, Option<String>)],
    checked_at: i64,
) -> Result<usize, String> {
    if rows.is_empty() {
        return Ok(0);
    }
    let tx = conn
        .transaction()
        .map_err(|e| format!("Failed to start Plex album artwork transaction: {e}"))?;
    let mut saved = 0usize;
    for (parent_rating_key, artwork_path) in rows {
        saved = saved.saturating_add(
            tx.execute(
                "INSERT INTO plex_cache_album_artwork(
                     server_id,parent_rating_key,artwork_path,checked_at
                 ) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(server_id,parent_rating_key) DO UPDATE SET
                     artwork_path=excluded.artwork_path,
                     checked_at=excluded.checked_at",
                params![server_id, parent_rating_key, artwork_path, checked_at],
            )
            .map_err(|e| format!("Failed to save Plex album artwork: {e}"))?,
        );
    }
    tx.commit()
        .map_err(|e| format!("Failed to commit Plex album artwork: {e}"))?;
    Ok(saved)
}

/// Persist successful metadata responses. `None` is intentional negative
/// caching, not an error: it prevents a genuinely coverless album from being
/// fetched on every application start.
pub fn plex_cache_save_album_artwork(
    server_id: Option<String>,
    rows: Vec<(String, Option<String>)>,
) -> Result<usize, String> {
    let mut conn = open_plex_cache_db()?;
    let server_id = server_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default");
    save_album_artwork_in(&mut conn, server_id, &rows, now_epoch_secs())
}

pub fn plex_cache_get_tracks(
    section_key: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<PlexTrack>, String> {
    let conn = open_plex_cache_db()?;
    let max = limit.unwrap_or(200) as i64;
    let mut tracks = Vec::new();

    if let Some(section) = section_key {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT rating_key, title, artist, album, duration_ms, artwork_path, part_key, container,
                        codec, channels, bitrate_kbps, sampling_rate_hz, bit_depth, track_number, disc_number,
                        year, genre, genres_json, parent_rating_key, {EFFECTIVE_COLLECTION_ARTWORK_SQL}, recording_mbid
                 FROM plex_cache_tracks
                 WHERE section_key = ?1
                 ORDER BY artist COLLATE NOCASE, album COLLATE NOCASE, disc_number, track_number, title COLLATE NOCASE
                 LIMIT ?2",
            ))
            .map_err(|e| format!("Failed to prepare Plex cache tracks query: {}", e))?;
        let rows = stmt
            .query_map(params![section, max], |row| {
                Ok(PlexTrack {
                    rating_key: row.get(0)?,
                    title: decode_xml_entities(row.get::<_, String>(1)?.trim()),
                    artist: row
                        .get::<_, Option<String>>(2)?
                        .map(|v| decode_xml_entities(v.trim())),
                    album: row
                        .get::<_, Option<String>>(3)?
                        .map(|v| decode_xml_entities(v.trim())),
                    duration_ms: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    artwork_path: row.get(5)?,
                    collection_artwork_path: row.get(19)?,
                    part_key: row.get(6)?,
                    container: row.get(7)?,
                    codec: row.get(8)?,
                    channels: row.get(9)?,
                    bitrate_kbps: row.get(10)?,
                    sampling_rate_hz: row.get(11)?,
                    bit_depth: row.get(12)?,
                    track_number: row.get(13)?,
                    disc_number: row.get(14)?,
                    year: row.get::<_, Option<i64>>(15)?.map(|v| v as u32),
                    genres: plex_genres_from_json(
                        row.get::<_, Option<String>>(17)?.as_deref(),
                        row.get::<_, Option<String>>(16)?.as_deref(),
                    ),
                    genre: row.get(16)?,
                    parent_rating_key: row.get(18)?,
                recording_mbid: row.get(20)?,
                })
            })
            .map_err(|e| format!("Failed to query Plex cache tracks: {}", e))?;
        for row in rows {
            tracks.push(row.map_err(|e| format!("Failed to read Plex cache track row: {}", e))?);
        }
    } else {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT rating_key, title, artist, album, duration_ms, artwork_path, part_key, container,
                        codec, channels, bitrate_kbps, sampling_rate_hz, bit_depth, track_number, disc_number,
                        year, genre, genres_json, parent_rating_key, {EFFECTIVE_COLLECTION_ARTWORK_SQL}, recording_mbid
                 FROM plex_cache_tracks
                 ORDER BY updated_at DESC
                 LIMIT ?1",
            ))
            .map_err(|e| format!("Failed to prepare Plex cache tracks query: {}", e))?;
        let rows = stmt
            .query_map(params![max], |row| {
                Ok(PlexTrack {
                    rating_key: row.get(0)?,
                    title: decode_xml_entities(row.get::<_, String>(1)?.trim()),
                    artist: row
                        .get::<_, Option<String>>(2)?
                        .map(|v| decode_xml_entities(v.trim())),
                    album: row
                        .get::<_, Option<String>>(3)?
                        .map(|v| decode_xml_entities(v.trim())),
                    duration_ms: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    artwork_path: row.get(5)?,
                    collection_artwork_path: row.get(19)?,
                    part_key: row.get(6)?,
                    container: row.get(7)?,
                    codec: row.get(8)?,
                    channels: row.get(9)?,
                    bitrate_kbps: row.get(10)?,
                    sampling_rate_hz: row.get(11)?,
                    bit_depth: row.get(12)?,
                    track_number: row.get(13)?,
                    disc_number: row.get(14)?,
                    year: row.get::<_, Option<i64>>(15)?.map(|v| v as u32),
                    genres: plex_genres_from_json(
                        row.get::<_, Option<String>>(17)?.as_deref(),
                        row.get::<_, Option<String>>(16)?.as_deref(),
                    ),
                    genre: row.get(16)?,
                    parent_rating_key: row.get(18)?,
                recording_mbid: row.get(20)?,
                })
            })
            .map_err(|e| format!("Failed to query Plex cache tracks: {}", e))?;
        for row in rows {
            tracks.push(row.map_err(|e| format!("Failed to read Plex cache track row: {}", e))?);
        }
    }

    Ok(tracks)
}

/// Hydrate metadata for a specific set of Plex tracks identified by
/// their ratingKey. Used by the playlist detail view to render Plex
/// rows that were added to a Qobuz-owned playlist (each row carries a
/// rating key and a position; this call fills in title / artist /
/// album / cover). Missing tracks (purged cache, never hydrated) are
/// silently omitted — the caller is responsible for graying out
/// positions that didn't come back.
pub fn plex_cache_get_tracks_by_keys(rating_keys: &[String]) -> Result<Vec<PlexTrack>, String> {
    if rating_keys.is_empty() {
        return Ok(Vec::new());
    }
    let conn = open_plex_cache_db()?;
    let placeholders = std::iter::repeat("?")
        .take(rating_keys.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT rating_key, title, artist, album, duration_ms, artwork_path, part_key, container,
                codec, channels, bitrate_kbps, sampling_rate_hz, bit_depth, track_number, disc_number,
                year, genre, genres_json, parent_rating_key, {EFFECTIVE_COLLECTION_ARTWORK_SQL}, recording_mbid
         FROM plex_cache_tracks
         WHERE rating_key IN ({placeholders})"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare Plex cache tracks query: {}", e))?;
    let params_iter = rusqlite::params_from_iter(rating_keys.iter());
    let rows = stmt
        .query_map(params_iter, |row| {
            Ok(PlexTrack {
                rating_key: row.get(0)?,
                title: decode_xml_entities(row.get::<_, String>(1)?.trim()),
                artist: row
                    .get::<_, Option<String>>(2)?
                    .map(|v| decode_xml_entities(v.trim())),
                album: row
                    .get::<_, Option<String>>(3)?
                    .map(|v| decode_xml_entities(v.trim())),
                duration_ms: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                artwork_path: row.get(5)?,
                collection_artwork_path: row.get(19)?,
                part_key: row.get(6)?,
                container: row.get(7)?,
                codec: row.get(8)?,
                channels: row.get(9)?,
                bitrate_kbps: row.get(10)?,
                sampling_rate_hz: row.get(11)?,
                bit_depth: row.get(12)?,
                track_number: row.get(13)?,
                disc_number: row.get(14)?,
                year: row.get::<_, Option<i64>>(15)?.map(|v| v as u32),
                genres: plex_genres_from_json(
                    row.get::<_, Option<String>>(17)?.as_deref(),
                    row.get::<_, Option<String>>(16)?.as_deref(),
                ),
                genre: row.get(16)?,
                parent_rating_key: row.get(18)?,
                recording_mbid: row.get(20)?,
            })
        })
        .map_err(|e| format!("Failed to query Plex cache tracks by keys: {}", e))?;
    let mut tracks = Vec::new();
    for row in rows {
        tracks.push(row.map_err(|e| format!("Failed to read Plex cache track row: {}", e))?);
    }
    Ok(tracks)
}

pub fn plex_cache_get_albums() -> Result<Vec<PlexCachedAlbum>, String> {
    let conn = open_plex_cache_db()?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT artist, album, duration_ms, artwork_path, {EFFECTIVE_COLLECTION_ARTWORK_SQL},
                    container, sampling_rate_hz, bit_depth, year, genre
             FROM plex_cache_tracks",
        ))
        .map_err(|e| {
            format!(
                "Failed to prepare Plex cache album aggregation query: {}",
                e
            )
        })?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })
        .map_err(|e| {
            format!(
                "Failed to query Plex cache tracks for album aggregation: {}",
                e
            )
        })?;

    let mut grouped: HashMap<String, PlexCachedAlbum> = HashMap::new();

    for row in rows {
        let (
            artist_opt,
            album_opt,
            duration_ms_opt,
            artwork_path,
            collection_artwork_path,
            container,
            sampling_rate_hz_opt,
            bit_depth_opt,
            year_opt,
            genre_opt,
        ) = row.map_err(|e| format!("Failed to read Plex cache aggregation row: {}", e))?;
        let artist = artist_opt
            .map(|v| decode_xml_entities(v.trim()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Unknown Artist".to_string());
        let album_raw = album_opt
            .map(|v| decode_xml_entities(v.trim()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Unknown Album".to_string());
        let album = normalize_album_title(Some(&artist), &album_raw);
        let album_key = plex_album_key(&artist, &album);
        let representative_artwork = collection_artwork_path
            .filter(|path| !path.is_empty())
            .or_else(|| artwork_path.clone().filter(|path| !path.is_empty()));

        let entry = grouped
            .entry(album_key.clone())
            .or_insert_with(|| PlexCachedAlbum {
                id: album_key.clone(),
                title: album.clone(),
                artist: artist.clone(),
                artwork_path: representative_artwork.clone(),
                track_count: 0,
                total_duration_secs: 0,
                format: container.clone().unwrap_or_else(|| "flac".to_string()),
                bit_depth: bit_depth_opt.map(|v| v as u32),
                sample_rate: sampling_rate_hz_opt.map(|v| v as u32).unwrap_or(44100),
                source: "plex".to_string(),
                likely_single_file_album: false,
                year: None,
                genre: None,
            });

        if entry.year.is_none() {
            entry.year = year_opt.map(|v| v as u32);
        }
        if entry.genre.is_none() {
            entry.genre = genre_opt.clone().filter(|s| !s.is_empty());
        }

        entry.track_count += 1;
        if let Some(duration_ms) = duration_ms_opt {
            entry.total_duration_secs += (duration_ms as u64) / 1000;
        }
        if entry.artwork_path.is_none() {
            entry.artwork_path = representative_artwork;
        }
        if let Some(container_value) = container {
            if entry.format == "flac" || entry.format.is_empty() {
                entry.format = container_value;
            }
        }
        if let Some(rate) = sampling_rate_hz_opt {
            let rate_u = rate as u32;
            if rate_u > entry.sample_rate {
                entry.sample_rate = rate_u;
            }
        }
        if let Some(depth) = bit_depth_opt {
            let depth_u = depth as u32;
            if depth_u > entry.bit_depth.unwrap_or(0) {
                entry.bit_depth = Some(depth_u);
            }
        }
    }

    let mut albums: Vec<PlexCachedAlbum> = grouped.into_values().collect();

    // Detect likely single-file albums (one track, duration > 10 minutes)
    for album in &mut albums {
        if album.track_count == 1 && album.total_duration_secs > 600 {
            album.likely_single_file_album = true;
        }
    }

    albums.sort_by(|a, b| {
        let artist_cmp = a.artist.to_lowercase().cmp(&b.artist.to_lowercase());
        if artist_cmp != std::cmp::Ordering::Equal {
            return artist_cmp;
        }
        a.title.to_lowercase().cmp(&b.title.to_lowercase())
    });
    Ok(albums)
}

/// Count of cached Plex tracks — a cheap `COUNT(*)` for the Local Library
/// Tracks-tab badge (so the count includes Plex alongside the albums/artists
/// badges, which already do). Avoids loading every row just to take a length.
pub fn plex_cache_count_tracks() -> Result<usize, String> {
    let conn = open_plex_cache_db()?;
    conn.query_row("SELECT COUNT(*) FROM plex_cache_tracks", [], |row| {
        row.get::<_, i64>(0)
    })
    .map(|n| n as usize)
    .map_err(|e| format!("Failed to count Plex cache tracks: {}", e))
}

/// Aggregate Plex artists client-side from the flat `plex_cache_tracks` table
/// (there is no `plex_cache_artists` table). Mirrors `plex_cache_get_albums`:
/// reads all rows, groups by normalized artist, counts distinct albums (by the
/// same `plex_album_key` the albums aggregator uses), sums track counts, and
/// keeps the first non-empty `artwork_path` as a representative portrait. The
/// returned `name` carries the decoded, display-ready spelling; counts feed the
/// Local Library artists rail's `LocalArtist`-shaped merge.
pub fn plex_cache_get_artists() -> Result<Vec<PlexCachedArtist>, String> {
    let conn = open_plex_cache_db()?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT artist, album, artwork_path, {EFFECTIVE_COLLECTION_ARTWORK_SQL}
               FROM plex_cache_tracks"
        ))
        .map_err(|e| {
            format!(
                "Failed to prepare Plex cache artist aggregation query: {}",
                e
            )
        })?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(|e| {
            format!(
                "Failed to query Plex cache tracks for artist aggregation: {}",
                e
            )
        })?;

    struct Acc {
        name: String,
        albums: std::collections::HashSet<String>,
        track_count: u32,
        artwork_path: Option<String>,
    }
    // Key by lowercase artist (matches the albums aggregator's case-folded
    // sort key); the first seen spelling is the display name.
    let mut grouped: HashMap<String, Acc> = HashMap::new();

    for row in rows {
        let (artist_opt, album_opt, artwork_path, collection_artwork_path) =
            row.map_err(|e| format!("Failed to read Plex cache artist row: {}", e))?;
        let artist = artist_opt
            .map(|v| decode_xml_entities(v.trim()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Unknown Artist".to_string());
        let album_raw = album_opt
            .map(|v| decode_xml_entities(v.trim()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Unknown Album".to_string());
        let album = normalize_album_title(Some(&artist), &album_raw);
        let album_key = plex_album_key(&artist, &album);

        let entry = grouped.entry(artist.to_lowercase()).or_insert_with(|| Acc {
            name: artist.clone(),
            albums: std::collections::HashSet::new(),
            track_count: 0,
            artwork_path: None,
        });
        entry.albums.insert(album_key);
        entry.track_count += 1;
        if entry.artwork_path.is_none() {
            if let Some(p) = artwork_path
                .filter(|p| !p.is_empty())
                .or_else(|| collection_artwork_path.filter(|p| !p.is_empty()))
            {
                entry.artwork_path = Some(p);
            }
        }
    }

    let mut artists: Vec<PlexCachedArtist> = grouped
        .into_values()
        .map(|a| PlexCachedArtist {
            name: a.name,
            album_count: a.albums.len() as u32,
            track_count: a.track_count,
            artwork_path: a.artwork_path,
        })
        .collect();
    artists.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(artists)
}

pub fn plex_cache_get_album_tracks(album_key: String) -> Result<Vec<PlexCachedTrack>, String> {
    let conn = open_plex_cache_db()?;
    plex_cache_get_album_tracks_in(&conn, &album_key)
}

fn plex_cache_get_album_tracks_in(
    conn: &Connection,
    album_key: &str,
) -> Result<Vec<PlexCachedTrack>, String> {
    // The derived catalog carries Plex's source-native parentRatingKey so an
    // edition can resolve directly. Legacy cards keep using the content hash.
    // Supporting both here preserves the old reader while avoiding a scan of
    // every cached album for the native F1 detail route.
    let (column, key) = album_key
        .strip_prefix("plex:album:")
        .map(|key| ("parent_rating_key", key))
        .unwrap_or(("album_key", album_key));
    let sql = format!(
        "SELECT rating_key, title, artist, album, duration_ms, container, bit_depth,
                sampling_rate_hz, artwork_path, track_number, disc_number, parent_rating_key,
                {EFFECTIVE_COLLECTION_ARTWORK_SQL}
           FROM plex_cache_tracks
          WHERE {column} = ?1
          ORDER BY disc_number, track_number, title COLLATE NOCASE"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare Plex cache album tracks query: {}", e))?;

    let rows = stmt
        .query_map(params![key], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
            ))
        })
        .map_err(|e| format!("Failed to query Plex cache album tracks: {}", e))?;

    let mut tracks = Vec::new();
    for row in rows {
        let (
            rating_key,
            title,
            artist_opt,
            album_opt,
            duration_ms_opt,
            container_opt,
            bit_depth_opt,
            sampling_rate_opt,
            artwork_path,
            track_number_opt,
            disc_number_opt,
            parent_rating_key,
            collection_artwork_path,
        ) = row.map_err(|e| format!("Failed to read Plex cache album track row: {}", e))?;
        let artist = artist_opt
            .map(|v| decode_xml_entities(v.trim()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Unknown Artist".to_string());
        let album_raw = album_opt
            .map(|v| decode_xml_entities(v.trim()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Unknown Album".to_string());
        let album = normalize_album_title(Some(&artist), &album_raw);
        tracks.push(PlexCachedTrack {
            id: playback_track_id(&rating_key),
            rating_key,
            title: decode_xml_entities(title.trim()),
            artist,
            album,
            duration_secs: duration_ms_opt.map(|v| (v as u64) / 1000).unwrap_or(0),
            format: container_opt.unwrap_or_else(|| "flac".to_string()),
            bit_depth: bit_depth_opt.map(|v| v as u32),
            sample_rate: sampling_rate_opt.map(|v| v as u32).unwrap_or(44100),
            artwork_path,
            collection_artwork_path,
            source: "plex".to_string(),
            album_key: album_key.to_string(),
            track_number: track_number_opt.map(|v| v as u32),
            disc_number: disc_number_opt.map(|v| v as u32),
            year: None,
            parent_rating_key,
        });
    }
    Ok(tracks)
}

pub fn plex_cache_search_tracks(
    query: String,
    limit: Option<u32>,
) -> Result<Vec<PlexCachedTrack>, String> {
    plex_cache_search_tracks_page(query, 0, limit.unwrap_or(u32::MAX) as u64, "default")
}

/// One deterministic Plex candidate page for a cross-source Tracks merge.
/// `None` on the compatibility helper now means unbounded instead of silently
/// hiding every track after row 5,000.
pub fn plex_cache_search_tracks_page(
    query: String,
    offset: u64,
    limit: u64,
    sort: &str,
) -> Result<Vec<PlexCachedTrack>, String> {
    plex_cache_search_tracks_page_filtered(query, offset, limit, sort, &[], false, &[])
}

pub fn plex_cache_search_tracks_page_filtered(
    query: String,
    offset: u64,
    limit: u64,
    sort: &str,
    formats: &[String],
    other_formats: bool,
    quality_tiers: &[String],
) -> Result<Vec<PlexCachedTrack>, String> {
    let conn = open_plex_cache_db()?;
    plex_cache_search_tracks_page_filtered_in(
        &conn,
        &query,
        offset,
        limit,
        sort,
        formats,
        other_formats,
        quality_tiers,
    )
}

#[cfg(test)]
fn plex_cache_search_tracks_page_in(
    conn: &Connection,
    query: &str,
    offset: u64,
    limit: u64,
    sort: &str,
) -> Result<Vec<PlexCachedTrack>, String> {
    plex_cache_search_tracks_page_filtered_in(conn, query, offset, limit, sort, &[], false, &[])
}

fn plex_cache_search_tracks_page_filtered_in(
    conn: &Connection,
    query: &str,
    offset: u64,
    limit: u64,
    sort: &str,
    formats: &[String],
    other_formats: bool,
    quality_tiers: &[String],
) -> Result<Vec<PlexCachedTrack>, String> {
    let order = match sort {
        "title-asc" => "sort_title COLLATE NOCASE, sort_artist COLLATE NOCASE, rating_key",
        "title-desc" => "sort_title COLLATE NOCASE DESC, sort_artist COLLATE NOCASE, rating_key",
        "artist-asc" => "sort_artist COLLATE NOCASE, sort_album COLLATE NOCASE, disc_number, track_number, rating_key",
        "artist-desc" => "sort_artist COLLATE NOCASE DESC, sort_album COLLATE NOCASE, disc_number, track_number, rating_key",
        "group-artist" => "sort_artist COLLATE NOCASE, sort_album COLLATE NOCASE, sort_title COLLATE NOCASE, rating_key",
        "year-desc" => "year IS NULL, year DESC, sort_album COLLATE NOCASE, disc_number, track_number, rating_key",
        "year-asc" => "year IS NULL, year ASC, sort_album COLLATE NOCASE, disc_number, track_number, rating_key",
        "added-desc" => "sort_album COLLATE NOCASE, disc_number, track_number, rating_key",
        _ => "sort_album COLLATE NOCASE, sort_artist COLLATE NOCASE, disc_number, track_number, sort_title COLLATE NOCASE, rating_key",
    };
    let media_filter = cached_track_media_filter_sql(
        "container",
        "bit_depth",
        "sampling_rate_hz",
        formats,
        other_formats,
        quality_tiers,
    );
    let needle = format!("%{}%", query.to_lowercase());
    let mut stmt = conn
        .prepare(&format!(
            "SELECT rating_key, title, artist, album, duration_ms, container, bit_depth,
                    sampling_rate_hz, artwork_path, track_number, disc_number, year,
                    {EFFECTIVE_COLLECTION_ARTWORK_SQL},
                    TRIM(title) AS sort_title,
                    COALESCE(NULLIF(TRIM(artist), ''), 'Unknown Artist') AS sort_artist,
                    CASE
                      WHEN TRIM(COALESCE(artist, '')) != ''
                       AND SUBSTR(TRIM(COALESCE(album, '')), 1, LENGTH(TRIM(artist)) + 3)
                           = TRIM(artist) || ' - '
                        THEN TRIM(SUBSTR(TRIM(album), LENGTH(TRIM(artist)) + 4))
                      WHEN TRIM(COALESCE(artist, '')) != ''
                       AND SUBSTR(TRIM(COALESCE(album, '')), 1, LENGTH(TRIM(artist)) + 3)
                           = TRIM(artist) || ' — '
                        THEN TRIM(SUBSTR(TRIM(album), LENGTH(TRIM(artist)) + 4))
                      WHEN TRIM(COALESCE(artist, '')) != ''
                       AND SUBSTR(TRIM(COALESCE(album, '')), 1, LENGTH(TRIM(artist)) + 3)
                           = TRIM(artist) || ' – '
                        THEN TRIM(SUBSTR(TRIM(album), LENGTH(TRIM(artist)) + 4))
                      WHEN TRIM(COALESCE(artist, '')) != ''
                       AND SUBSTR(TRIM(COALESCE(album, '')), 1, LENGTH(TRIM(artist)) + 2)
                           = TRIM(artist) || ': '
                        THEN TRIM(SUBSTR(TRIM(album), LENGTH(TRIM(artist)) + 3))
                      ELSE COALESCE(NULLIF(TRIM(album), ''), 'Unknown Album')
                    END AS sort_album
             FROM plex_cache_tracks
             WHERE (?1 = '' OR
                   lower(title) LIKE ?2 OR
                   lower(COALESCE(artist, '')) LIKE ?2 OR
                   lower(COALESCE(album, '')) LIKE ?2) \
             {media_filter}
             ORDER BY {order}
             LIMIT ?3 OFFSET ?4"
        ))
        .map_err(|e| format!("Failed to prepare Plex cache search query: {}", e))?;

    let rows = stmt
        .query_map(
            params![query.trim(), needle, limit as i64, offset as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                ))
            },
        )
        .map_err(|e| format!("Failed to query Plex cache search tracks: {}", e))?;

    let mut tracks = Vec::new();
    for row in rows {
        let (
            rating_key,
            title,
            artist_opt,
            album_opt,
            duration_ms_opt,
            container_opt,
            bit_depth_opt,
            sampling_rate_opt,
            artwork_path,
            track_number_opt,
            disc_number_opt,
            year_opt,
            collection_artwork_path,
        ) = row.map_err(|e| format!("Failed to read Plex cache search row: {}", e))?;
        let artist = artist_opt
            .map(|v| decode_xml_entities(v.trim()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Unknown Artist".to_string());
        let album_raw = album_opt
            .map(|v| decode_xml_entities(v.trim()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Unknown Album".to_string());
        let album = normalize_album_title(Some(&artist), &album_raw);
        tracks.push(PlexCachedTrack {
            id: playback_track_id(&rating_key),
            rating_key: rating_key.clone(),
            title: decode_xml_entities(title.trim()),
            artist: artist.clone(),
            album: album.clone(),
            duration_secs: duration_ms_opt.map(|v| (v as u64) / 1000).unwrap_or(0),
            format: container_opt.unwrap_or_else(|| "flac".to_string()),
            bit_depth: bit_depth_opt.map(|v| v as u32),
            sample_rate: sampling_rate_opt.map(|v| v as u32).unwrap_or(44100),
            artwork_path,
            collection_artwork_path,
            source: "plex".to_string(),
            album_key: plex_album_key(&artist, &album),
            track_number: track_number_opt.map(|v| v as u32),
            disc_number: disc_number_opt.map(|v| v as u32),
            year: year_opt.map(|v| v as u32),
            // Flat search list — version grouping doesn't apply here.
            parent_rating_key: None,
        });
    }
    Ok(tracks)
}

fn cached_track_media_filter_sql(
    format_col: &str,
    depth_col: &str,
    rate_col: &str,
    formats: &[String],
    other_formats: bool,
    quality_tiers: &[String],
) -> String {
    let known = ["flac", "alac", "ape", "wav", "mp3", "aac"];
    let selected = known
        .iter()
        .copied()
        .filter(|value| {
            formats
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(value))
        })
        .collect::<Vec<_>>();
    let mut clauses = Vec::new();
    if !selected.is_empty() || other_formats {
        let mut values = selected
            .into_iter()
            .map(|value| format!("LOWER(COALESCE({format_col},''))='{value}'"))
            .collect::<Vec<_>>();
        if other_formats {
            values.push(format!(
                "LOWER(COALESCE({format_col},'')) NOT IN ('flac','alac','ape','wav','mp3','aac')"
            ));
        }
        clauses.push(format!("AND ({})", values.join(" OR ")));
    }
    let hires = quality_tiers.iter().any(|value| value == "hires");
    let cd = quality_tiers.iter().any(|value| value == "cd");
    let lossy = quality_tiers.iter().any(|value| value == "lossy");
    if hires || cd || lossy {
        let khz = format!(
            "CASE WHEN COALESCE({rate_col},0)>=1000 THEN COALESCE({rate_col},0)/1000.0 ELSE COALESCE({rate_col},0) END"
        );
        let mut values = Vec::new();
        if hires {
            values.push(format!(
                "(LOWER(COALESCE({format_col},'')) IN ('dsd','dsf','dff') OR COALESCE({depth_col},0)>=24)"
            ));
        }
        if cd {
            values.push(format!(
                "(LOWER(COALESCE({format_col},'')) NOT IN ('mp3','dsd','dsf','dff') AND (({depth_col} IS NOT NULL AND {depth_col}<24) OR ({depth_col} IS NULL AND {khz}>=44.1)))"
            ));
        }
        if lossy {
            values.push(format!("LOWER(COALESCE({format_col},''))='mp3'"));
        }
        clauses.push(format!("AND ({})", values.join(" OR ")));
    }
    clauses.join(" ")
}

/// Bulk by-rating-key lookup in the `PlexCachedTrack` shape (synthetic
/// playback id + normalized album + content-hash album key — the same
/// row mapping `plex_cache_search_tracks` produces, so a row resolved
/// here is indistinguishable from a Local-Library-merged one). Used by
/// the local-playlist detail to hydrate `plex_key` membership rows.
/// Keys missing from the cache are silently omitted — the caller renders
/// those rows as explicitly unavailable.
pub fn plex_cache_get_cached_tracks_by_keys(
    rating_keys: &[String],
) -> Result<Vec<PlexCachedTrack>, String> {
    if rating_keys.is_empty() {
        return Ok(Vec::new());
    }
    let conn = open_plex_cache_db()?;
    let placeholders = std::iter::repeat("?")
        .take(rating_keys.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT rating_key, title, artist, album, duration_ms, container, bit_depth,
                sampling_rate_hz, artwork_path, track_number, disc_number, parent_rating_key,
                {EFFECTIVE_COLLECTION_ARTWORK_SQL}
         FROM plex_cache_tracks
         WHERE rating_key IN ({placeholders})"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare Plex cache by-keys query: {}", e))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(rating_keys.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
            ))
        })
        .map_err(|e| format!("Failed to query Plex cache tracks by keys: {}", e))?;
    let mut tracks = Vec::new();
    for row in rows {
        let (
            rating_key,
            title,
            artist_opt,
            album_opt,
            duration_ms_opt,
            container_opt,
            bit_depth_opt,
            sampling_rate_opt,
            artwork_path,
            track_number_opt,
            disc_number_opt,
            parent_rating_key,
            collection_artwork_path,
        ) = row.map_err(|e| format!("Failed to read Plex cache by-keys row: {}", e))?;
        let artist = artist_opt
            .map(|v| decode_xml_entities(v.trim()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Unknown Artist".to_string());
        let album_raw = album_opt
            .map(|v| decode_xml_entities(v.trim()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Unknown Album".to_string());
        let album = normalize_album_title(Some(&artist), &album_raw);
        tracks.push(PlexCachedTrack {
            id: playback_track_id(&rating_key),
            rating_key,
            title: decode_xml_entities(title.trim()),
            artist: artist.clone(),
            album: album.clone(),
            duration_secs: duration_ms_opt.map(|v| (v as u64) / 1000).unwrap_or(0),
            format: container_opt.unwrap_or_else(|| "flac".to_string()),
            bit_depth: bit_depth_opt.map(|v| v as u32),
            sample_rate: sampling_rate_opt.map(|v| v as u32).unwrap_or(44100),
            artwork_path,
            collection_artwork_path,
            source: "plex".to_string(),
            album_key: plex_album_key(&artist, &album),
            track_number: track_number_opt.map(|v| v as u32),
            disc_number: disc_number_opt.map(|v| v as u32),
            year: None,
            parent_rating_key,
        });
    }
    Ok(tracks)
}

#[derive(Debug)]
struct StoredSectionSync {
    state: PlexSectionSyncState,
    server_id: Option<String>,
    status: String,
}

fn nonnegative_u64(value: i64) -> u64 {
    value.max(0) as u64
}

fn load_section_sync_in(
    conn: &Connection,
    section_key: &str,
) -> Result<Option<StoredSectionSync>, String> {
    conn.query_row(
        "SELECT server_id,generation,next_start,total_size,observed_rows,status
           FROM plex_cache_section_sync WHERE section_key=?1",
        params![section_key],
        |row| {
            Ok(StoredSectionSync {
                state: PlexSectionSyncState {
                    section_key: section_key.to_string(),
                    generation: nonnegative_u64(row.get(1)?),
                    next_start: nonnegative_u64(row.get(2)?),
                    total_size: row.get::<_, Option<i64>>(3)?.map(nonnegative_u64),
                    observed_rows: nonnegative_u64(row.get(4)?),
                    resumed: false,
                },
                server_id: row.get(0)?,
                status: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(|e| format!("Failed to read Plex section sync state: {}", e))
}

fn begin_section_sync_in(
    conn: &mut Connection,
    server_id: Option<String>,
    section_key: &str,
) -> Result<PlexSectionSyncState, String> {
    let tx = conn
        .transaction()
        .map_err(|e| format!("Failed to start Plex section sync transaction: {}", e))?;
    let previous = load_section_sync_in(&tx, section_key)?;
    let resumable = previous.as_ref().is_some_and(|stored| {
        stored.server_id == server_id
            && matches!(
                stored.status.as_str(),
                "running" | "interrupted" | "cancelled"
            )
    });
    if resumable {
        let mut state = previous.expect("checked above").state;
        state.resumed = true;
        tx.execute(
            "UPDATE plex_cache_section_sync
                SET status='running',updated_at=?2 WHERE section_key=?1",
            params![section_key, now_epoch_secs()],
        )
        .map_err(|e| format!("Failed to resume Plex section sync: {}", e))?;
        tx.commit()
            .map_err(|e| format!("Failed to commit Plex section resume: {}", e))?;
        return Ok(state);
    }

    let generation = previous
        .map(|stored| stored.state.generation)
        .unwrap_or(0)
        .saturating_add(1)
        .min(i64::MAX as u64)
        .max(1);
    tx.execute(
        "INSERT INTO plex_cache_section_sync(
             section_key,server_id,generation,next_start,total_size,observed_rows,status,updated_at
         ) VALUES (?1,?2,?3,0,NULL,0,'running',?4)
         ON CONFLICT(section_key) DO UPDATE SET
             server_id=excluded.server_id,generation=excluded.generation,next_start=0,
             total_size=NULL,observed_rows=0,status='running',updated_at=excluded.updated_at",
        params![section_key, server_id, generation as i64, now_epoch_secs()],
    )
    .map_err(|e| format!("Failed to begin Plex section sync: {}", e))?;
    tx.commit()
        .map_err(|e| format!("Failed to commit Plex section start: {}", e))?;
    Ok(PlexSectionSyncState {
        section_key: section_key.to_string(),
        generation,
        next_start: 0,
        total_size: None,
        observed_rows: 0,
        resumed: false,
    })
}

pub fn plex_cache_begin_section_sync(
    server_id: Option<String>,
    section_key: String,
) -> Result<PlexSectionSyncState, String> {
    let mut conn = open_plex_cache_db()?;
    begin_section_sync_in(&mut conn, server_id, &section_key)
}

fn apply_section_page_in(
    conn: &mut Connection,
    section_key: &str,
    generation: u64,
    page: &PlexTrackPage,
) -> Result<PlexSectionSyncState, String> {
    if page.response_size != page.tracks.len() as u64 {
        return Err("Plex page track count does not match its response size".to_string());
    }
    let tx = conn
        .transaction()
        .map_err(|e| format!("Failed to start Plex page transaction: {}", e))?;
    let stored = load_section_sync_in(&tx, section_key)?
        .ok_or_else(|| "Plex section sync state is missing".to_string())?;
    if stored.status != "running" || stored.state.generation != generation {
        return Err("Plex section generation is no longer current".to_string());
    }
    if stored.state.next_start != page.offset {
        return Err(format!(
            "Plex section checkpoint mismatch: expected {}, received {}",
            stored.state.next_start, page.offset
        ));
    }
    if stored
        .state
        .total_size
        .is_some_and(|total| total != page.total_size)
    {
        return Err("Plex section totalSize changed during sync".to_string());
    }

    let now = now_epoch_secs();
    let generation_i64 = generation.min(i64::MAX as u64) as i64;
    let mut insert = tx
        .prepare_cached(
            "INSERT INTO plex_cache_tracks(
                 rating_key,section_key,server_id,title,artist,album,duration_ms,artwork_path,
                 collection_artwork_path,part_key,container,codec,channels,bitrate_kbps,
                 sampling_rate_hz,bit_depth,
                 track_number,disc_number,album_key,year,genre,genres_json,parent_rating_key,updated_at,
                 sync_generation,recording_mbid
             ) VALUES (
                 ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,
                 ?19,?20,?21,?22,?23,?24,?25,?26
             )
             ON CONFLICT(rating_key) DO UPDATE SET
                 recording_mbid=COALESCE(excluded.recording_mbid,plex_cache_tracks.recording_mbid),
                 section_key=excluded.section_key,server_id=excluded.server_id,
                 title=excluded.title,artist=excluded.artist,album=excluded.album,
                 duration_ms=excluded.duration_ms,artwork_path=excluded.artwork_path,
                 collection_artwork_path=excluded.collection_artwork_path,
                 part_key=excluded.part_key,
                 container=COALESCE(excluded.container,plex_cache_tracks.container),
                 codec=COALESCE(excluded.codec,plex_cache_tracks.codec),
                 channels=COALESCE(excluded.channels,plex_cache_tracks.channels),
                 bitrate_kbps=COALESCE(excluded.bitrate_kbps,plex_cache_tracks.bitrate_kbps),
                 sampling_rate_hz=COALESCE(
                     excluded.sampling_rate_hz,plex_cache_tracks.sampling_rate_hz
                 ),
                 bit_depth=COALESCE(excluded.bit_depth,plex_cache_tracks.bit_depth),
                 track_number=excluded.track_number,disc_number=excluded.disc_number,
                 album_key=excluded.album_key,year=excluded.year,genre=excluded.genre,
                 genres_json=excluded.genres_json,
                 parent_rating_key=excluded.parent_rating_key,updated_at=excluded.updated_at,
                 sync_generation=excluded.sync_generation",
        )
        .map_err(|e| format!("Failed to prepare Plex page upsert: {}", e))?;
    for track in &page.tracks {
        let genres_json = plex_genres_json(track);
        let artist = track
            .artist
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Unknown Artist");
        let album_raw = track
            .album
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Unknown Album");
        let album = normalize_album_title(Some(artist), album_raw);
        insert
            .execute(params![
                track.rating_key,
                section_key,
                stored.server_id,
                track.title,
                track.artist,
                track.album,
                track.duration_ms.map(|value| value as i64),
                track.artwork_path,
                track.collection_artwork_path,
                track.part_key,
                track.container,
                track.codec,
                track.channels.map(|value| value as i64),
                track.bitrate_kbps.map(|value| value as i64),
                track.sampling_rate_hz.map(|value| value as i64),
                track.bit_depth.map(|value| value as i64),
                track.track_number.map(|value| value as i64),
                track.disc_number.map(|value| value as i64),
                plex_album_key(artist, &album),
                track.year.map(|value| value as i64),
                track.genre,
                genres_json,
                track.parent_rating_key,
                now,
                generation_i64,
                track.recording_mbid.clone(),
            ])
            .map_err(|e| format!("Failed to upsert Plex page track: {}", e))?;
    }
    drop(insert);

    let observed_rows = stored
        .state
        .observed_rows
        .saturating_add(page.tracks.len() as u64);
    let actual: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM plex_cache_tracks
              WHERE section_key=?1 AND sync_generation=?2",
            params![section_key, generation_i64],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to verify Plex page identities: {}", e))?;
    if nonnegative_u64(actual) != observed_rows {
        return Err("Plex section repeated a ratingKey across pages".to_string());
    }
    let next_start = page.next_start();
    tx.execute(
        "UPDATE plex_cache_section_sync
            SET next_start=?3,total_size=?4,observed_rows=?5,status='running',updated_at=?6
          WHERE section_key=?1 AND generation=?2",
        params![
            section_key,
            generation_i64,
            next_start.min(i64::MAX as u64) as i64,
            page.total_size.min(i64::MAX as u64) as i64,
            observed_rows.min(i64::MAX as u64) as i64,
            now,
        ],
    )
    .map_err(|e| format!("Failed to checkpoint Plex page: {}", e))?;
    tx.commit()
        .map_err(|e| format!("Failed to commit Plex page: {}", e))?;
    Ok(PlexSectionSyncState {
        section_key: section_key.to_string(),
        generation,
        next_start,
        total_size: Some(page.total_size),
        observed_rows,
        resumed: stored.state.resumed,
    })
}

pub fn plex_cache_apply_section_page(
    section_key: String,
    generation: u64,
    page: PlexTrackPage,
) -> Result<PlexSectionSyncState, String> {
    let mut conn = open_plex_cache_db()?;
    apply_section_page_in(&mut conn, &section_key, generation, &page)
}

fn finish_section_sync_in(
    conn: &mut Connection,
    section_key: &str,
    generation: u64,
) -> Result<usize, String> {
    let tx = conn
        .transaction()
        .map_err(|e| format!("Failed to start Plex section completion: {}", e))?;
    let stored = load_section_sync_in(&tx, section_key)?
        .ok_or_else(|| "Plex section sync state is missing".to_string())?;
    let total = stored
        .state
        .total_size
        .ok_or_else(|| "Plex section totalSize is unknown".to_string())?;
    if stored.status != "running"
        || stored.state.generation != generation
        || stored.state.next_start != total
        || stored.state.observed_rows != total
    {
        return Err("Plex section is incomplete and cannot authorize prune".to_string());
    }
    let generation_i64 = generation.min(i64::MAX as u64) as i64;
    let actual: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM plex_cache_tracks
              WHERE section_key=?1 AND sync_generation=?2",
            params![section_key, generation_i64],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to verify Plex section completion: {}", e))?;
    if nonnegative_u64(actual) != total {
        return Err("Plex section identity count does not match totalSize".to_string());
    }
    let pruned = tx
        .execute(
            "DELETE FROM plex_cache_tracks
              WHERE section_key=?1 AND sync_generation<>?2",
            params![section_key, generation_i64],
        )
        .map_err(|e| format!("Failed to prune completed Plex section: {}", e))?;
    tx.execute(
        "UPDATE plex_cache_section_sync
            SET status='complete',updated_at=?3 WHERE section_key=?1 AND generation=?2",
        params![section_key, generation_i64, now_epoch_secs()],
    )
    .map_err(|e| format!("Failed to complete Plex section sync: {}", e))?;
    tx.commit()
        .map_err(|e| format!("Failed to commit Plex section completion: {}", e))?;
    Ok(pruned)
}

pub fn plex_cache_finish_section_sync(
    section_key: String,
    generation: u64,
) -> Result<usize, String> {
    let mut conn = open_plex_cache_db()?;
    finish_section_sync_in(&mut conn, &section_key, generation)
}

fn mark_section_sync_in(
    conn: &Connection,
    section_key: &str,
    generation: u64,
    status: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE plex_cache_section_sync SET status=?3,updated_at=?4
          WHERE section_key=?1 AND generation=?2 AND status='running'",
        params![
            section_key,
            generation.min(i64::MAX as u64) as i64,
            status,
            now_epoch_secs(),
        ],
    )
    .map(|_| ())
    .map_err(|e| format!("Failed to mark Plex section sync: {}", e))
}

pub fn plex_cache_interrupt_section_sync(
    section_key: String,
    generation: u64,
) -> Result<(), String> {
    let conn = open_plex_cache_db()?;
    mark_section_sync_in(&conn, &section_key, generation, "interrupted")
}

pub fn plex_cache_restart_section_sync(section_key: String, generation: u64) -> Result<(), String> {
    let conn = open_plex_cache_db()?;
    mark_section_sync_in(&conn, &section_key, generation, "restart")
}

pub fn plex_cache_save_tracks(
    server_id: Option<String>,
    section_key: String,
    tracks: Vec<PlexTrack>,
) -> Result<usize, String> {
    let mut conn = open_plex_cache_db()?;
    let tx = conn
        .transaction()
        .map_err(|e| format!("Failed to start Plex cache tracks transaction: {}", e))?;

    // Save existing hydrated quality data before clearing the section
    let mut hydrated_quality: HashMap<String, (Option<String>, Option<i64>, Option<i64>)> =
        HashMap::new();
    {
        let mut stmt = tx
            .prepare(
                "SELECT rating_key, container, sampling_rate_hz, bit_depth
                 FROM plex_cache_tracks
                 WHERE section_key = ?1 AND (sampling_rate_hz IS NOT NULL OR bit_depth IS NOT NULL)",
            )
            .map_err(|e| format!("Failed to prepare hydrated quality query: {}", e))?;
        let rows = stmt
            .query_map(params![section_key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })
            .map_err(|e| format!("Failed to read hydrated quality data: {}", e))?;
        for row in rows {
            let (key, container, rate, depth) =
                row.map_err(|e| format!("Failed to read hydrated quality row: {}", e))?;
            hydrated_quality.insert(key, (container, rate, depth));
        }
    }

    tx.execute(
        "DELETE FROM plex_cache_tracks WHERE section_key = ?1",
        params![section_key],
    )
    .map_err(|e| format!("Failed to clear old Plex cache tracks for section: {}", e))?;

    let now = now_epoch_secs();
    for track in &tracks {
        // Use hydrated quality if bulk data has NULL and we have previously hydrated values
        let saved = hydrated_quality.get(&track.rating_key);
        let container = track
            .container
            .as_ref()
            .cloned()
            .or_else(|| saved.and_then(|s| s.0.clone()));
        let sampling_rate_hz = track
            .sampling_rate_hz
            .map(|v| v as i64)
            .or_else(|| saved.and_then(|s| s.1));
        let bit_depth = track
            .bit_depth
            .map(|v| v as i64)
            .or_else(|| saved.and_then(|s| s.2));

        // Compute album_key for this track
        let track_artist = track
            .artist
            .as_deref()
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .unwrap_or("Unknown Artist");
        let track_album_raw = track
            .album
            .as_deref()
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .unwrap_or("Unknown Album");
        let track_album_normalized = normalize_album_title(Some(track_artist), track_album_raw);
        let track_album_key = plex_album_key(track_artist, &track_album_normalized);

        tx.execute(
            "INSERT INTO plex_cache_tracks
             (rating_key, section_key, server_id, title, artist, album, duration_ms, artwork_path,
              collection_artwork_path, part_key, container, codec, channels, bitrate_kbps,
              sampling_rate_hz, bit_depth,
              track_number, disc_number, album_key, year, genre, genres_json, parent_rating_key, updated_at,
              recording_mbid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            params![
                track.rating_key,
                section_key,
                server_id,
                track.title,
                track.artist,
                track.album,
                track.duration_ms.map(|v| v as i64),
                track.artwork_path,
                track.collection_artwork_path,
                track.part_key,
                container,
                track.codec,
                track.channels.map(|v| v as i64),
                track.bitrate_kbps.map(|v| v as i64),
                sampling_rate_hz,
                bit_depth,
                track.track_number.map(|v| v as i64),
                track.disc_number.map(|v| v as i64),
                track_album_key,
                track.year.map(|v| v as i64),
                track.genre.clone(),
                plex_genres_json(track),
                track.parent_rating_key.clone(),
                now,
                track.recording_mbid.clone(),
            ],
        )
        .map_err(|e| format!("Failed to insert Plex cache track: {}", e))?;
    }

    tx.commit()
        .map_err(|e| format!("Failed to commit Plex cache tracks transaction: {}", e))?;
    Ok(tracks.len())
}

pub fn plex_cache_update_track_quality(
    updates: Vec<PlexTrackQualityUpdate>,
) -> Result<usize, String> {
    if updates.is_empty() {
        return Ok(0);
    }

    let mut conn = open_plex_cache_db()?;
    let tx = conn.transaction().map_err(|e| {
        format!(
            "Failed to start Plex cache quality update transaction: {}",
            e
        )
    })?;

    let now = now_epoch_secs();
    let mut updated_rows = 0usize;
    for update in &updates {
        let affected = tx
            .execute(
                "UPDATE plex_cache_tracks
                 SET container = COALESCE(?2, container),
                     sampling_rate_hz = COALESCE(?3, sampling_rate_hz),
                     bit_depth = COALESCE(?4, bit_depth),
                     updated_at = ?5
                 WHERE rating_key = ?1",
                params![
                    update.rating_key,
                    update.container,
                    update.sampling_rate_hz.map(|v| v as i64),
                    update.bit_depth.map(|v| v as i64),
                    now,
                ],
            )
            .map_err(|e| format!("Failed to update Plex cache track quality: {}", e))?;
        updated_rows += affected;
    }

    tx.commit().map_err(|e| {
        format!(
            "Failed to commit Plex cache quality update transaction: {}",
            e
        )
    })?;

    Ok(updated_rows)
}

pub fn plex_cache_get_tracks_needing_hydration(limit: Option<u32>) -> Result<Vec<String>, String> {
    let conn = open_plex_cache_db()?;
    let max = limit.unwrap_or(50) as i64;
    let mut stmt = conn
        .prepare(
            "SELECT rating_key FROM plex_cache_tracks
             WHERE sampling_rate_hz IS NULL OR bit_depth IS NULL
             LIMIT ?1",
        )
        .map_err(|e| format!("Failed to prepare hydration query: {}", e))?;
    let rows = stmt
        .query_map(params![max], |row| row.get::<_, String>(0))
        .map_err(|e| format!("Failed to query tracks needing hydration: {}", e))?;
    let mut keys = Vec::new();
    for row in rows {
        keys.push(row.map_err(|e| format!("Failed to read hydration row: {}", e))?);
    }
    Ok(keys)
}

pub fn plex_cache_clear() -> Result<(), String> {
    let mut conn = open_plex_cache_db()?;
    let tx = conn
        .transaction()
        .map_err(|e| format!("Failed to start Plex cache clear: {}", e))?;
    tx.execute("DELETE FROM plex_cache_tracks", [])
        .map_err(|e| format!("Failed to clear Plex cache tracks: {}", e))?;
    tx.execute("DELETE FROM plex_cache_sections", [])
        .map_err(|e| format!("Failed to clear Plex cache sections: {}", e))?;
    tx.execute("DELETE FROM plex_cache_section_sync", [])
        .map_err(|e| format!("Failed to clear Plex sync state: {}", e))?;
    tx.execute("DELETE FROM plex_cache_album_artwork", [])
        .map_err(|e| format!("Failed to clear Plex album artwork cache: {}", e))?;
    tx.commit()
        .map_err(|e| format!("Failed to commit Plex cache clear: {}", e))?;
    Ok(())
}

/// Remove cached tracks for sections NOT in `keep`, leaving the kept sections
/// (and their hydrated quality) intact. Use this before a re-sync instead of
/// the full `plex_cache_clear`: a full wipe deletes the rows BEFORE
/// `plex_cache_save_tracks` can read them for its hydration carry-over, so
/// already-hydrated albums lose their bit-depth/sample-rate on every re-sync.
/// Pruning only the de-selected sections preserves the kept sections' quality
/// (and `save_tracks` then re-applies its per-section carry-over). An empty
/// `keep` removes all tracks (nothing selected). Returns rows deleted.
pub fn plex_cache_prune_sections(keep: &[String]) -> Result<usize, String> {
    let mut conn = open_plex_cache_db()?;
    let tx = conn
        .transaction()
        .map_err(|e| format!("Failed to start Plex section selection prune: {}", e))?;
    if keep.is_empty() {
        let removed = tx
            .execute("DELETE FROM plex_cache_tracks", [])
            .map_err(|e| format!("Failed to prune Plex cache tracks: {}", e))?;
        tx.execute("DELETE FROM plex_cache_section_sync", [])
            .map_err(|e| format!("Failed to prune Plex sync state: {}", e))?;
        tx.execute("DELETE FROM plex_cache_album_artwork", [])
            .map_err(|e| format!("Failed to prune Plex album artwork cache: {}", e))?;
        tx.commit()
            .map_err(|e| format!("Failed to commit Plex section selection prune: {}", e))?;
        return Ok(removed);
    }
    let placeholders = keep.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let track_sql =
        format!("DELETE FROM plex_cache_tracks WHERE section_key NOT IN ({placeholders})");
    let state_sql =
        format!("DELETE FROM plex_cache_section_sync WHERE section_key NOT IN ({placeholders})");
    let params: Vec<&dyn rusqlite::ToSql> =
        keep.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
    let removed = tx
        .execute(&track_sql, params.as_slice())
        .map_err(|e| format!("Failed to prune Plex cache tracks: {}", e))?;
    tx.execute(&state_sql, params.as_slice())
        .map_err(|e| format!("Failed to prune Plex sync state: {}", e))?;
    tx.execute(
        "DELETE FROM plex_cache_album_artwork
          WHERE NOT EXISTS (
              SELECT 1 FROM plex_cache_tracks tracks
               WHERE COALESCE(NULLIF(tracks.server_id,''),'default') =
                     plex_cache_album_artwork.server_id
                 AND tracks.parent_rating_key = plex_cache_album_artwork.parent_rating_key
          )",
        [],
    )
    .map_err(|e| format!("Failed to prune orphan Plex album artwork: {}", e))?;
    tx.commit()
        .map_err(|e| format!("Failed to commit Plex section selection prune: {}", e))?;
    Ok(removed)
}

pub async fn plex_resolve_track_media(
    base_url: String,
    token: String,
    rating_key: String,
) -> Result<PlexResolvedMedia, String> {
    let client = build_plex_client()?;
    let base = normalize_base_url(&base_url);

    let metadata_url = with_token(&format!("{base}/library/metadata/{rating_key}"), &token);
    let metadata_xml = client
        .get(metadata_url)
        .send()
        .await
        .map_err(|e| safe_http_error("Plex metadata request", &e))?
        .error_for_status()
        .map_err(|e| safe_http_error("Plex metadata request", &e))?
        .text()
        .await
        .map_err(|e| safe_http_error("Failed to read Plex metadata response", &e))?;

    let mut tracks = parse_tracks(&metadata_xml, Some(1));
    let track = tracks
        .pop()
        .ok_or_else(|| format!("Track {rating_key} not found in Plex metadata"))?;

    let part_key = track
        .part_key
        .clone()
        .ok_or_else(|| format!("Track {rating_key} does not include a playable Part key"))?;

    let part_url = with_token(&as_download(&format!("{base}{part_key}")), &token);
    let part_response = client
        .get(&part_url)
        .send()
        .await
        .map_err(|e| safe_http_error("Plex part request", &e))?
        .error_for_status()
        .map_err(|e| safe_http_error("Plex part request", &e))?;

    let content_type = part_response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let bytes = part_response
        .bytes()
        .await
        .map_err(|e| safe_http_error("Failed to read Plex media bytes", &e))?;

    let playback_id = playback_track_id(&rating_key);
    Ok(PlexResolvedMedia {
        rating_key,
        playback_id,
        part_key: part_key.clone(),
        part_url,
        bytes: bytes.to_vec(),
        direct_play_confirmed: is_direct_part_key(&part_key),
        content_type,
        sampling_rate_hz: track.sampling_rate_hz,
        bit_depth: track.bit_depth,
    })
}

/// Direct-play part location resolved WITHOUT downloading the body.
///
/// `plex_resolve_part_url` returns this so the player's progressive streaming
/// feeder can Range-stream the original on-disk bytes (bit-perfect, ~1s to
/// first audio) instead of buffering the whole FLAC into RAM first. Cast/DLNA
/// keep using `plex_resolve_track_media` (full body).
#[derive(Debug, Clone)]
pub struct PlexPartLocation {
    pub rating_key: String,
    pub playback_id: u64,
    pub part_key: String,
    pub part_url: String,
    pub direct_play_confirmed: bool,
    pub content_type: Option<String>,
    pub sampling_rate_hz: Option<u32>,
    pub bit_depth: Option<u32>,
    /// The non-secret request identity required when another crate fetches
    /// `part_url` instead of using qbz-plex's preconfigured client.
    pub request_headers: Vec<(String, String)>,
}

/// Resolve the direct-play part URL for a Plex track WITHOUT downloading the
/// body. Same metadata GET + `with_token` derivation as
/// `plex_resolve_track_media`, but stops at the URL — no `.bytes()`. The
/// streaming feeder then Range-streams the original bytes progressively, so the
/// FLAC stays bit-perfect. `content_type` is taken from the metadata container
/// (e.g. `"flac"`) to avoid an extra round-trip; it is display-only (the feeder
/// sniffs the format from the FLAC magic bytes in its Range probe).
pub async fn plex_resolve_part_url(
    base_url: String,
    token: String,
    rating_key: String,
) -> Result<PlexPartLocation, String> {
    let client = build_plex_client()?;
    let base = normalize_base_url(&base_url);

    let metadata_url = with_token(&format!("{base}/library/metadata/{rating_key}"), &token);
    let metadata_xml = client
        .get(metadata_url)
        .send()
        .await
        .map_err(|e| safe_http_error("Plex metadata request", &e))?
        .error_for_status()
        .map_err(|e| safe_http_error("Plex metadata request", &e))?
        .text()
        .await
        .map_err(|e| safe_http_error("Failed to read Plex metadata response", &e))?;

    let mut tracks = parse_tracks(&metadata_xml, Some(1));
    let track = tracks
        .pop()
        .ok_or_else(|| format!("Track {rating_key} not found in Plex metadata"))?;

    let part_key = track
        .part_key
        .clone()
        .ok_or_else(|| format!("Track {rating_key} does not include a playable Part key"))?;

    let part_url = with_token(&as_download(&format!("{base}{part_key}")), &token);
    let playback_id = playback_track_id(&rating_key);

    Ok(PlexPartLocation {
        rating_key,
        playback_id,
        direct_play_confirmed: is_direct_part_key(&part_key),
        part_key,
        part_url,
        content_type: track.container.clone(),
        sampling_rate_hz: track.sampling_rate_hz,
        bit_depth: track.bit_depth,
        request_headers: plex_media_request_headers(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plex_part_download_flag_preserves_existing_query_parameters() {
        assert_eq!(
            as_download("http://plex/library/parts/7/file.flac"),
            "http://plex/library/parts/7/file.flac?download=1"
        );
        assert_eq!(
            as_download("http://plex/library/parts/7/file.flac?foo=bar"),
            "http://plex/library/parts/7/file.flac?foo=bar&download=1"
        );
        assert_eq!(
            as_download("http://plex/library/parts/7/file.flac?download=1&foo=bar"),
            "http://plex/library/parts/7/file.flac?download=1&foo=bar"
        );
        assert_eq!(
            with_token(
                &as_download("http://plex/library/parts/7/file.flac?foo=bar"),
                "secret"
            ),
            "http://plex/library/parts/7/file.flac?foo=bar&download=1&X-Plex-Token=secret"
        );
    }

    fn search_db(track_count: u32) -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE plex_cache_tracks (
                rating_key TEXT PRIMARY KEY, title TEXT NOT NULL, artist TEXT,
                album TEXT, duration_ms INTEGER, container TEXT, bit_depth INTEGER,
                sampling_rate_hz INTEGER, artwork_path TEXT, collection_artwork_path TEXT,
                server_id TEXT, parent_rating_key TEXT,
                track_number INTEGER,
                disc_number INTEGER, year INTEGER
            );
            CREATE TABLE plex_cache_album_artwork (
                server_id TEXT NOT NULL, parent_rating_key TEXT NOT NULL,
                artwork_path TEXT, checked_at INTEGER NOT NULL,
                PRIMARY KEY(server_id,parent_rating_key)
            );",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO plex_cache_tracks
                   (rating_key,title,artist,album,duration_ms,container,bit_depth,
                    sampling_rate_hz,track_number,disc_number,year)
                 VALUES (?1,?2,'Plex Artist',?3,1000,'flac',24,96000,?4,1,2026)",
                )
                .unwrap();
            for i in 0..track_count {
                insert
                    .execute(params![
                        (i + 1).to_string(),
                        format!("Track {i:05}"),
                        format!("Album {:04}", i / 10),
                        (i % 10 + 1) as i64,
                    ])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
        conn
    }

    fn sync_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE plex_cache_tracks (
                 rating_key TEXT PRIMARY KEY,
                 section_key TEXT NOT NULL,
                 server_id TEXT,
                 title TEXT NOT NULL,
                 artist TEXT,
                 album TEXT,
                 duration_ms INTEGER,
                 artwork_path TEXT,
                 collection_artwork_path TEXT,
                 part_key TEXT,
                 container TEXT,
                 codec TEXT,
                 channels INTEGER,
                 bitrate_kbps INTEGER,
                 sampling_rate_hz INTEGER,
                 bit_depth INTEGER,
                 track_number INTEGER,
                 disc_number INTEGER,
                 album_key TEXT,
                 year INTEGER,
                 genre TEXT,
                 genres_json TEXT NOT NULL DEFAULT '[]',
                 parent_rating_key TEXT,
                 updated_at INTEGER NOT NULL,
                 sync_generation INTEGER NOT NULL DEFAULT 0,
                 recording_mbid TEXT
             );
             CREATE INDEX idx_sync_tracks_section ON plex_cache_tracks(section_key);
             CREATE TABLE plex_cache_album_artwork (
                 server_id TEXT NOT NULL,
                 parent_rating_key TEXT NOT NULL,
                 artwork_path TEXT,
                 checked_at INTEGER NOT NULL,
                 PRIMARY KEY(server_id,parent_rating_key)
             );
             CREATE TABLE plex_cache_section_sync (
                 section_key TEXT PRIMARY KEY,
                 server_id TEXT,
                 generation INTEGER NOT NULL DEFAULT 0,
                 next_start INTEGER NOT NULL DEFAULT 0,
                 total_size INTEGER,
                 observed_rows INTEGER NOT NULL DEFAULT 0,
                 status TEXT NOT NULL DEFAULT 'idle',
                 updated_at INTEGER NOT NULL DEFAULT 0
             );",
        )
        .unwrap();
        conn
    }

    fn sync_track(index: u64, with_quality: bool) -> PlexTrack {
        PlexTrack {
            rating_key: (index + 1).to_string(),
            title: format!("Track {index:05}"),
            artist: Some("Plex Artist".to_string()),
            album: Some(format!("Album {:04}", index / 10)),
            duration_ms: Some(180_000),
            artwork_path: Some(format!("/library/metadata/{}/thumb", index + 1)),
            collection_artwork_path: Some(format!("/library/metadata/album-{}/thumb", index / 10)),
            part_key: Some(format!("/library/parts/{}/file.flac", index + 1)),
            container: with_quality.then(|| "flac".to_string()),
            codec: with_quality.then(|| "flac".to_string()),
            channels: with_quality.then_some(2),
            bitrate_kbps: with_quality.then_some(2_800),
            sampling_rate_hz: with_quality.then_some(192_000),
            bit_depth: with_quality.then_some(24),
            track_number: Some((index % 10 + 1) as u32),
            disc_number: Some(1),
            year: Some(2026),
            genres: vec!["Fixture".to_string()],
            genre: Some("Fixture".to_string()),
            parent_rating_key: Some(format!("album-{}", index / 10)),
            recording_mbid: None,
        }
    }

    fn sync_page(start: u64, end: u64, total: u64, with_quality: bool) -> PlexTrackPage {
        PlexTrackPage {
            tracks: (start..end)
                .map(|index| sync_track(index, with_quality))
                .collect(),
            offset: start,
            response_size: end - start,
            total_size: total,
        }
    }

    fn page_xml(start: u64, end: u64, total: u64) -> String {
        let mut xml = format!(
            "<MediaContainer offset=\"{start}\" size=\"{}\" totalSize=\"{total}\">",
            end - start
        );
        for index in start..end {
            xml.push_str(&format!(
                "<Track ratingKey=\"{}\" title=\"Track {index:05}\" grandparentTitle=\"Artist\" parentTitle=\"Album\" duration=\"180000\"/>",
                index + 1
            ));
        }
        xml.push_str("</MediaContainer>");
        xml
    }

    #[test]
    fn parses_music_sections() {
        let xml = r#"<MediaContainer>
            <Directory key="1" title="Music" type="artist"/>
            <Directory key="2" title="Movies" type="movie"/>
        </MediaContainer>"#;
        let sections = parse_music_sections(xml);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].key, "1");
    }

    #[test]
    fn album_artwork_uses_the_metadata_token_instead_of_inventing_one() {
        let xml = r#"<MediaContainer size="1">
            <Directory ratingKey="60335" type="album" title="1994"
                thumb="/library/metadata/60320/thumb/1665204207">
                <Image type="coverPoster" url="/library/metadata/60320/thumb/1665204207"/>
            </Directory>
        </MediaContainer>"#;
        assert_eq!(
            parse_album_artwork_path(xml).as_deref(),
            Some("/library/metadata/60320/thumb/1665204207")
        );
        assert_ne!(
            parse_album_artwork_path(xml).as_deref(),
            Some("/library/metadata/60335/thumb")
        );
    }

    #[test]
    fn album_artwork_cache_is_server_scoped_negative_cached_and_resolved() {
        let mut conn = sync_db();
        conn.execute(
            "INSERT INTO plex_cache_tracks(
                 rating_key,section_key,server_id,title,artist,album,parent_rating_key,
                 updated_at,sync_generation
             ) VALUES ('track-1','music','server-a','Song','Artist','Album','album-1',1,1)",
            [],
        )
        .unwrap();
        assert_eq!(
            album_artwork_candidates_in(&conn, "server-a", 100).unwrap(),
            ["album-1"]
        );
        assert!(album_artwork_candidates_in(&conn, "server-b", 100)
            .unwrap()
            .is_empty());

        save_album_artwork_in(
            &mut conn,
            "server-a",
            &[(
                "album-1".into(),
                Some("/library/metadata/cover/thumb/7".into()),
            )],
            200,
        )
        .unwrap();
        assert!(album_artwork_candidates_in(&conn, "server-a", 199)
            .unwrap()
            .is_empty());
        let effective: Option<String> = conn
            .query_row(
                &format!(
                    "SELECT {EFFECTIVE_COLLECTION_ARTWORK_SQL}
                       FROM plex_cache_tracks WHERE rating_key='track-1'"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            effective.as_deref(),
            Some("/library/metadata/cover/thumb/7")
        );

        save_album_artwork_in(&mut conn, "server-a", &[("album-1".into(), None)], 300).unwrap();
        let effective: Option<String> = conn
            .query_row(
                &format!(
                    "SELECT {EFFECTIVE_COLLECTION_ARTWORK_SQL}
                       FROM plex_cache_tracks WHERE rating_key='track-1'"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(effective, None);
    }

    #[test]
    fn parses_tracks_with_stream_audio_metadata() {
        let xml = r#"<MediaContainer>
            <Track ratingKey="42" title="Song" grandparentTitle="Artist" parentTitle="Album" duration="123000" thumb="/library/metadata/42/thumb/1" parentThumb="/library/metadata/album-9/thumb/1" genre="Progressive Rock">
                <Media container="flac">
                    <Part key="/library/parts/999/file.flac"/>
                    <Stream streamType="2" codecType="audio" codec="flac" channels="2" samplingRate="96000" bitDepth="24" bitrate="3120"/>
                </Media>
                <Genre tag="Art Rock"/>
                <Genre tag="progressive rock"/>
            </Track>
        </MediaContainer>"#;
        let tracks = parse_tracks(xml, Some(10));
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].rating_key, "42");
        assert_eq!(
            tracks[0].part_key.as_deref(),
            Some("/library/parts/999/file.flac")
        );
        assert_eq!(
            tracks[0].artwork_path.as_deref(),
            Some("/library/metadata/42/thumb/1")
        );
        assert_eq!(
            tracks[0].collection_artwork_path.as_deref(),
            Some("/library/metadata/album-9/thumb/1")
        );
        assert_eq!(tracks[0].sampling_rate_hz, Some(96000));
        assert_eq!(tracks[0].bit_depth, Some(24));
        assert_eq!(tracks[0].genres, ["Progressive Rock", "Art Rock"]);
        assert_eq!(tracks[0].genre.as_deref(), Some("Progressive Rock"));
    }

    #[test]
    fn plex_filtered_query_applies_format_and_quality_before_pagination() {
        let conn = search_db(17);
        conn.execute(
            "UPDATE plex_cache_tracks SET container='mp3',bit_depth=NULL,sampling_rate_hz=44100
             WHERE CAST(rating_key AS INTEGER) % 2 = 1",
            [],
        )
        .unwrap();
        let formats = vec!["mp3".to_string()];
        let qualities = vec!["lossy".to_string()];
        let first = plex_cache_search_tracks_page_filtered_in(
            &conn,
            "",
            0,
            4,
            "title-asc",
            &formats,
            false,
            &qualities,
        )
        .unwrap();
        let second = plex_cache_search_tracks_page_filtered_in(
            &conn,
            "",
            4,
            8,
            "title-asc",
            &formats,
            false,
            &qualities,
        )
        .unwrap();
        let ids = first
            .iter()
            .chain(&second)
            .map(|row| row.rating_key.clone())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 9);
        assert!(first.iter().chain(&second).all(|row| row.format == "mp3"));
    }

    #[test]
    fn parses_bounded_container_page_and_rejects_incomplete_xml() {
        let xml = r#"<MediaContainer offset="500" size="2" totalSize="503">
            <Track ratingKey="501" title="One"/>
            <Track ratingKey="502" title="Two"/>
        </MediaContainer>"#;
        let page = parse_track_page(xml, 500, 500).unwrap();
        assert_eq!(page.offset, 500);
        assert_eq!(page.response_size, 2);
        assert_eq!(page.total_size, 503);
        assert_eq!(page.next_start(), 502);
        assert!(page.has_more());

        let truncated = r#"<MediaContainer offset="500" size="2" totalSize="503">
            <Track ratingKey="501" title="One"/>
        </MediaContainer>"#;
        assert!(parse_track_page(truncated, 500, 500).is_err());
    }

    #[test]
    fn paged_section_over_five_thousand_resumes_and_prunes_only_at_completion() {
        let mut conn = sync_db();
        let first = begin_section_sync_in(&mut conn, Some("server-a".into()), "music").unwrap();
        assert_eq!(first.generation, 1);
        for start in (0..5_137_u64).step_by(DEFAULT_PLEX_TRACK_PAGE_SIZE as usize) {
            let end = (start + DEFAULT_PLEX_TRACK_PAGE_SIZE).min(5_137);
            apply_section_page_in(
                &mut conn,
                "music",
                first.generation,
                &sync_page(start, end, 5_137, true),
            )
            .unwrap();
        }
        assert_eq!(
            finish_section_sync_in(&mut conn, "music", first.generation).unwrap(),
            0
        );

        let second = begin_section_sync_in(&mut conn, Some("server-a".into()), "music").unwrap();
        assert_eq!(second.generation, 2);
        let mut state = second.clone();
        for start in [0_u64, DEFAULT_PLEX_TRACK_PAGE_SIZE] {
            state = apply_section_page_in(
                &mut conn,
                "music",
                second.generation,
                &sync_page(start, start + DEFAULT_PLEX_TRACK_PAGE_SIZE, 5_005, false),
            )
            .unwrap();
        }
        mark_section_sync_in(&conn, "music", second.generation, "interrupted").unwrap();
        let before_resume: i64 = conn
            .query_row("SELECT COUNT(*) FROM plex_cache_tracks", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            before_resume, 5_137,
            "an incomplete generation never prunes"
        );

        let resumed = begin_section_sync_in(&mut conn, Some("server-a".into()), "music").unwrap();
        assert!(resumed.resumed);
        assert_eq!(resumed.generation, second.generation);
        assert_eq!(resumed.next_start, state.next_start);
        for start in (resumed.next_start..5_005).step_by(DEFAULT_PLEX_TRACK_PAGE_SIZE as usize) {
            let end = (start + DEFAULT_PLEX_TRACK_PAGE_SIZE).min(5_005);
            state = apply_section_page_in(
                &mut conn,
                "music",
                resumed.generation,
                &sync_page(start, end, 5_005, false),
            )
            .unwrap();
        }
        assert_eq!(state.observed_rows, 5_005);
        assert_eq!(
            finish_section_sync_in(&mut conn, "music", resumed.generation).unwrap(),
            132
        );
        let counts: (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*),COUNT(DISTINCT rating_key) FROM plex_cache_tracks",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (5_005, 5_005));
        let quality: (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT sampling_rate_hz,bit_depth FROM plex_cache_tracks WHERE rating_key='1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(quality, (Some(192_000), Some(24)));
    }

    #[test]
    fn repeated_rating_key_across_pages_does_not_advance_checkpoint() {
        let mut conn = sync_db();
        let state = begin_section_sync_in(&mut conn, None, "music").unwrap();
        let first = sync_page(0, 1, 2, false);
        let state = apply_section_page_in(&mut conn, "music", state.generation, &first).unwrap();
        let duplicate = PlexTrackPage {
            tracks: vec![sync_track(0, false)],
            offset: 1,
            response_size: 1,
            total_size: 2,
        };
        assert!(apply_section_page_in(&mut conn, "music", state.generation, &duplicate).is_err());
        let stored = load_section_sync_in(&conn, "music").unwrap().unwrap();
        assert_eq!(stored.state.next_start, 1);
        assert_eq!(stored.state.observed_rows, 1);
        assert!(finish_section_sync_in(&mut conn, "music", state.generation).is_err());
    }

    #[test]
    fn changed_total_size_requires_a_fresh_generation() {
        let mut conn = sync_db();
        let first = begin_section_sync_in(&mut conn, Some("server-a".into()), "music").unwrap();
        let checkpoint = apply_section_page_in(
            &mut conn,
            "music",
            first.generation,
            &sync_page(0, 1, 2, false),
        )
        .unwrap();
        assert_eq!(checkpoint.next_start, 1);

        let changed_total = sync_page(1, 2, 3, false);
        assert!(
            apply_section_page_in(&mut conn, "music", first.generation, &changed_total,).is_err()
        );
        let unchanged = load_section_sync_in(&conn, "music").unwrap().unwrap();
        assert_eq!(unchanged.state.next_start, 1);
        assert_eq!(unchanged.state.total_size, Some(2));

        mark_section_sync_in(&conn, "music", first.generation, "restart").unwrap();
        let restarted = begin_section_sync_in(&mut conn, Some("server-a".into()), "music").unwrap();
        assert_eq!(restarted.generation, first.generation + 1);
        assert_eq!(restarted.next_start, 0);
        assert_eq!(restarted.total_size, None);
        assert!(!restarted.resumed);
    }

    #[test]
    fn page_transaction_failure_rolls_back_rows_and_checkpoint() {
        let mut conn = sync_db();
        conn.execute_batch(
            "CREATE TRIGGER fail_second_track
             BEFORE INSERT ON plex_cache_tracks
             WHEN NEW.rating_key='2'
             BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END;",
        )
        .unwrap();
        let state = begin_section_sync_in(&mut conn, None, "music").unwrap();
        assert!(apply_section_page_in(
            &mut conn,
            "music",
            state.generation,
            &sync_page(0, 2, 2, false),
        )
        .is_err());
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM plex_cache_tracks", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
        let stored = load_section_sync_in(&conn, "music").unwrap().unwrap();
        assert_eq!(stored.state.next_start, 0);
        assert_eq!(stored.state.observed_rows, 0);
    }

    #[test]
    fn paged_xml_metric_keeps_peak_document_bounded() {
        const TRACKS: u64 = 5_137;
        let full_xml_bytes = page_xml(0, TRACKS, TRACKS).len();
        let started = std::time::Instant::now();
        let mut max_page_bytes = 0_usize;
        let mut parsed = 0_u64;
        let mut pages = 0_u64;
        for start in (0..TRACKS).step_by(DEFAULT_PLEX_TRACK_PAGE_SIZE as usize) {
            let end = (start + DEFAULT_PLEX_TRACK_PAGE_SIZE).min(TRACKS);
            let xml = page_xml(start, end, TRACKS);
            max_page_bytes = max_page_bytes.max(xml.len());
            let page = parse_track_page(&xml, start, DEFAULT_PLEX_TRACK_PAGE_SIZE).unwrap();
            parsed += page.tracks.len() as u64;
            pages += 1;
        }
        let elapsed = started.elapsed();
        assert_eq!(parsed, TRACKS);
        assert!(max_page_bytes * 8 < full_xml_bytes);
        println!(
            "H_PLEX_METRIC tracks={TRACKS} pages={pages} full_xml_bytes={full_xml_bytes} max_page_bytes={max_page_bytes} parse_ms={}",
            elapsed.as_millis(),
        );
    }

    #[test]
    fn direct_part_key_detection() {
        assert!(is_direct_part_key("/library/parts/1234/file.flac"));
        assert!(!is_direct_part_key("/music/:/transcode/universal/start"));
    }

    #[test]
    fn playback_track_id_prefers_numeric_rating_key() {
        assert_eq!(playback_track_id("48012"), 48012);
    }

    #[test]
    fn plex_search_pages_past_five_thousand_without_omissions() {
        let conn = search_db(5_137);
        let mut offset = 0;
        let mut ids = std::collections::HashSet::new();
        loop {
            let page =
                plex_cache_search_tracks_page_in(&conn, "Track", offset, 500, "title-asc").unwrap();
            if page.is_empty() {
                break;
            }
            for track in &page {
                assert!(ids.insert(track.id), "duplicate Plex id {}", track.id);
            }
            offset += page.len() as u64;
        }
        assert_eq!(offset, 5_137);
        assert_eq!(ids.len(), 5_137);
    }

    #[test]
    fn plex_page_order_uses_the_published_album_and_year_values() {
        let conn = search_db(2);
        conn.execute(
            "UPDATE plex_cache_tracks
                SET title = 'Zulu', album = 'Plex Artist - Alpha', year = 2020
              WHERE rating_key = '1'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE plex_cache_tracks
                SET title = 'Alpha', album = 'Beta', year = NULL
              WHERE rating_key = '2'",
            [],
        )
        .unwrap();

        let default = plex_cache_search_tracks_page_in(&conn, "", 0, 10, "default").unwrap();
        assert_eq!(default[0].rating_key, "1");
        assert_eq!(default[0].album, "Alpha");

        let title = plex_cache_search_tracks_page_in(&conn, "", 0, 10, "title-asc").unwrap();
        assert_eq!(title[0].rating_key, "2");

        let year = plex_cache_search_tracks_page_in(&conn, "", 0, 10, "year-desc").unwrap();
        assert_eq!(year[0].rating_key, "1");
        assert_eq!(year[0].year, Some(2020));
        assert_eq!(year[1].year, None);
    }

    #[test]
    fn plex_artist_group_orders_by_the_performing_artist() {
        let conn = search_db(2);
        conn.execute(
            "UPDATE plex_cache_tracks SET artist = 'Zed Performer', album = 'A Album', title = 'First' WHERE rating_key = '1'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE plex_cache_tracks SET artist = 'Alpha Performer', album = 'Z Album', title = 'Second' WHERE rating_key = '2'",
            [],
        )
        .unwrap();

        let grouped = plex_cache_search_tracks_page_in(&conn, "", 0, 10, "group-artist").unwrap();
        assert_eq!(grouped[0].artist, "Alpha Performer");
        assert_eq!(grouped[1].artist, "Zed Performer");
    }

    #[test]
    fn album_detail_resolves_content_hash_and_native_parent_key() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE plex_cache_tracks (
                rating_key TEXT PRIMARY KEY, title TEXT NOT NULL, artist TEXT,
                album TEXT, duration_ms INTEGER, container TEXT, bit_depth INTEGER,
                sampling_rate_hz INTEGER, artwork_path TEXT, collection_artwork_path TEXT,
                server_id TEXT,
                track_number INTEGER,
                disc_number INTEGER, album_key TEXT, parent_rating_key TEXT
            );
            CREATE TABLE plex_cache_album_artwork (
                server_id TEXT NOT NULL, parent_rating_key TEXT NOT NULL,
                artwork_path TEXT, checked_at INTEGER NOT NULL,
                PRIMARY KEY(server_id,parent_rating_key)
            );
            INSERT INTO plex_cache_tracks
                (rating_key,title,artist,album,duration_ms,container,bit_depth,
                 sampling_rate_hz,track_number,disc_number,album_key,parent_rating_key)
            VALUES
                ('1','First','Artist','Album',180000,'flac',24,96000,1,1,'plex:hash','parent-a'),
                ('2','Second','Artist','Album',180000,'flac',24,96000,2,1,'plex:hash','parent-a'),
                ('3','Other edition','Artist','Album',180000,'flac',16,44100,1,1,'plex:hash','parent-b');",
        )
        .unwrap();

        let legacy = plex_cache_get_album_tracks_in(&conn, "plex:hash").unwrap();
        assert_eq!(legacy.len(), 3);
        let native = plex_cache_get_album_tracks_in(&conn, "plex:album:parent-a").unwrap();
        assert_eq!(
            native
                .iter()
                .map(|track| track.rating_key.as_str())
                .collect::<Vec<_>>(),
            vec!["1", "2"]
        );
        assert!(native
            .iter()
            .all(|track| track.album_key == "plex:album:parent-a"));
    }
}
