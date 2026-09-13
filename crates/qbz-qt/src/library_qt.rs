//! Library view data layer — Slint-free port of `crates/qbz/src/favorites.rs`
//! (per-tab loaders) + `crates/qbz/src/library_all.rs` (the mixed All feed).
//!
//! One fetch fans out to every source (Qobuz favorites paged per type,
//! playlists, purchases, the local-favorites store), normalizes each into a
//! `FeedItem`, and merges the All feed by the recency proxy
//! (`added_rank = index / len` per source, stable sort — library_all.rs).
//!
//! Transport: ONE JSON document on the bridge (`libraryJson`); the QML side
//! parses once and derives tabs/search/sort/source-filters in JS (the Slint
//! app derives in Rust because Slint JS is weak — QML JS is real; the
//! JSON.parse cost at owner scale is MEASURED and reported, per the phase
//! brief). Artwork is id-keyed via the `libraryArtworkReady` signal (see
//! bridge.rs) so a cover never lands on the wrong row and decoded-cover RAM
//! scales with the viewport.
//!
//! Known deltas:
//! - The artist/album blacklist store is live, but this feed does not yet
//!   apply it while assembling rows.
//! - The Slint offline-only rail of playable cached favourites is not mounted;
//!   offline Library currently shows its placeholder instead.
//! - Local scope: BOTH branches are live. "favorites" (hearted local items)
//!   comes from the LocalFavoritesService; "all" (the whole local library +
//!   Plex) is `all_local_feed_blocking` below, over the same `library.db`
//!   queries the Local Library tabs run. The Settings row that picks between
//!   them is Local Library > LIBRARY › ALL (`library_prefs::local_scope`).
//! - Playlist follow/copy affordances: a feed row's heart is the qbz-LOCAL
//!   library.db flag (`toggle_playlist_favorite` — Qobuz has no
//!   `playlist_ids` favorite param); subscribe/unsubscribe of a FOREIGN
//!   playlist only exists on the playlist page (`playlist_qt::toggle_follow`),
//!   not on the feed rows.
//! Genre filtering, the local-scope preference, multiselect, grouping and A-Z
//! navigation are all live in `LibraryView.qml`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use futures_util::stream::{self, StreamExt};
use qbz_app::settings::local_favorites::{LocalFavItem, LocalFavoritesService, DB_FILE_NAME};
use qbz_app::shell::AppRuntime;
use qbz_app::user_data::UserDataPaths;
use qbz_core::LoggingAdapter;
use qbz_models::{Album, Artist, Playlist, QueueTrack, Track};
use serde::Serialize;

use crate::home_qt;

/// Matches favorites.rs paging.
const PAGE_SIZE: u32 = 500;

/// One normalized row (superset of the per-tab cards + the All feed item).
#[derive(Clone, Default, Serialize)]
pub struct FeedItem {
    pub kind: String,   // track | album | artist | playlist | label
    pub group: String,  // favorites | following | purchases | local
    pub source: String, // qobuz | local | plex
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub artist: String,
    #[serde(rename = "artistId")]
    pub artist_id: String,
    pub album: String,
    #[serde(rename = "albumId")]
    pub album_id: String,
    /// Track rows only: the release's MAIN artist name. `artist` is the
    /// per-track performer and can differ on classical/features; replacement
    /// release ranking must compare like with like.
    #[serde(
        default,
        rename = "albumArtist",
        skip_serializing_if = "String::is_empty"
    )]
    pub album_artist: String,
    /// Track rows only: the id of the ALBUM's main artist, beside the
    /// performer id in `artist_id`. A favourited track is a member of an
    /// artist's "In library" tab through EITHER — `artist_id` alone is the
    /// track's performer, which on features, interpreters and re-credited
    /// remasters is not the artist whose page is open (issue #737 round,
    /// contract §5). Skipped when empty so album/artist rows pay nothing.
    #[serde(
        default,
        rename = "albumArtistId",
        skip_serializing_if = "String::is_empty"
    )]
    pub album_artist_id: String,
    #[serde(rename = "imageUrl")]
    pub image_url: String,
    /// Stable artwork key (`{kind}:{id}`) — library_all.rs `feed_key`.
    #[serde(rename = "artKey")]
    pub art_key: String,
    #[serde(rename = "qualityTier")]
    pub quality_tier: String,
    #[serde(rename = "qualityDetail")]
    pub quality_detail: String,
    /// RAW catalog max bit depth / sample rate (kHz) — the SAME two numbers
    /// `quality_detail` above is derived from.
    ///
    /// THE CONTRACT (reference: `crates/qbz/src/playback.rs:2426`
    /// `make_queue_track`): a queue track carries the NUMBERS. Both feed ->
    /// queue builders (`feed_track_to_queue` below and
    /// `playback_qt::feed_queue_track`) map THIS row into a `QueueTrack`, so
    /// without the fields they hardcoded `None` and every Library-feed track
    /// play left the NPB AudioStamp with a tier and no detail line.
    #[serde(rename = "bitDepth", skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
    #[serde(rename = "sampleRate", skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<f64>,
    #[serde(rename = "isFavorite")]
    pub is_favorite: bool,
    /// Qobuz PULLED this track or album (`streamable: false` — contract §5.1).
    /// Meaningless on artist / playlist rows and skipped when false, so the
    /// ~10k-row feed JSON does not grow a key per row.
    ///
    /// BOTH feed -> queue builders read it (`feed_track_to_queue` below and
    /// `playback_qt::feed_queue_track`), for the same reason they both read
    /// `bit_depth`: the two must not disagree about what a row is.
    #[serde(
        default,
        rename = "qobuzUnavailable",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub not_streamable: bool,
    /// This track's owning Qobuz release is itself confirmed withdrawn. False
    /// also covers UNKNOWN: transport/auth/parse failures must not hide a
    /// navigation action or expose destructive release-wide cleanup.
    #[serde(
        default,
        rename = "releaseUnavailable",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub release_unavailable: bool,
    /// The persisted local-favorite snapshot no longer resolves in the
    /// active Local Library.  Kept separate from `not_streamable`: a Qobuz
    /// withdrawal may still play from a complete download, while a missing
    /// local/Plex source must remain a non-navigable tombstone.
    #[serde(
        default,
        rename = "sourceUnavailable",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub source_unavailable: bool,
    /// Liveable exception to `not_streamable`: a complete offline copy still
    /// plays after Qobuz withdraws the catalog row. Zero is omitted so the
    /// high-cardinality feed only pays for tracks that actually carry state.
    #[serde(default, rename = "cacheStatus", skip_serializing_if = "is_zero_i32")]
    pub cache_status: i32,
    pub genre: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, rename = "updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(default, rename = "trackCount", skip_serializing_if = "Option::is_none")]
    pub track_count: Option<u32>,
    pub year: String,
    #[serde(
        default,
        rename = "releaseSortKey",
        skip_serializing_if = "is_zero_u32"
    )]
    pub release_sort_key: u32,
    pub duration: String,
    /// Matching evidence carried by Qobuz favourite-track rows. Omitted for
    /// every other feed kind so the high-cardinality mixed document only pays
    /// for data the replacement picker can actually use.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub isrc: String,
    #[serde(default, rename = "durationSecs", skip_serializing_if = "is_zero_u32")]
    pub duration_secs: u32,
    pub explicit: bool,
    #[serde(rename = "playlistOwned")]
    pub playlist_owned: bool,
    #[serde(
        default,
        rename = "playlistCreator",
        skip_serializing_if = "String::is_empty"
    )]
    pub playlist_creator: String,
    #[serde(rename = "playlistFollowing")]
    pub playlist_following: bool,
    #[serde(rename = "isPinned", default)]
    pub is_pinned: bool,
    /// Playlist rows only: up to four de-duplicated member-track covers —
    /// the 2x2 mosaic the card renders when the playlist has no artwork of
    /// its OWN (Tauri `FavoritePlaylistCard` -> `PlaylistCollage`, Slint
    /// `PlaylistCollage.slint`). Skipped when empty so the ~10k-row feed
    /// JSON does not grow an `"covers":[]` per non-playlist row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covers: Vec<String>,
    /// True when `image_url` is the playlist's OWN Qobuz artwork
    /// (`image_rectangle`) instead of a member-track cover. The card then
    /// renders it CONTAIN — those graphics are landscape and cropping
    /// butchers them (Tauri `QobuzPlaylistCard`: `object-fit: contain`).
    #[serde(rename = "playlistOwnImage", default, skip_serializing_if = "is_false")]
    pub playlist_own_image: bool,
    /// Recency proxy in [0,1]; 0 = most-recently added (per source).
    pub added_rank: f32,
}

/// `skip_serializing_if` predicate for the default-false flags above.
fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn is_zero_i32(value: &i32) -> bool {
    *value == 0
}

impl FeedItem {
    fn keyed(mut self) -> Self {
        self.art_key = feed_key(&self.kind, &self.id);
        self
    }
}

/// library_all.rs `feed_key` — Qobuz numeric ids overlap across entity
/// types, so the kind prefixes the key.
pub fn feed_key(kind: &str, id: &str) -> String {
    format!("{kind}:{id}")
}

#[derive(Clone, Serialize)]
pub struct LibraryCounts {
    pub tracks: i64,
    pub albums: i64,
    pub artists: i64,
    pub playlists: i64,
    pub labels: i64,
    pub all: i64,
}

pub struct LibraryData {
    pub feed: Vec<FeedItem>,
    pub counts: LibraryCounts,
}

static LIBRARY: Mutex<Option<LibraryData>> = Mutex::new(None);

// Successful owned deletions are authoritative for this session, even when an
// already running list request returns the old playlist afterwards. Lock order:
// deleted IDs -> Library/sidebar/manager cache; never the reverse.
static DELETED_PLAYLISTS: std::sync::LazyLock<Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));
static PLAYLIST_DELETION_REVISION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub(crate) fn playlist_deletion_revision() -> u64 {
    PLAYLIST_DELETION_REVISION.load(std::sync::atomic::Ordering::SeqCst)
}

pub(crate) fn with_deleted_playlists<R>(
    f: impl FnOnce(&std::collections::HashSet<String>) -> R,
) -> R {
    f(&DELETED_PLAYLISTS.lock().unwrap())
}

fn exclude_deleted_playlists(data: &mut LibraryData, deleted: &std::collections::HashSet<String>) {
    let before = data.feed.len();
    let mut removed_favorites = std::collections::HashSet::new();
    data.feed.retain(|row| {
        let remove = row.kind == "playlist" && deleted.contains(&row.id);
        if remove && (row.playlist_owned || row.is_favorite) {
            removed_favorites.insert(row.id.clone());
        }
        !remove
    });
    data.counts.all = (data.counts.all - (before - data.feed.len()) as i64).max(0);
    data.counts.playlists = (data.counts.playlists - removed_favorites.len() as i64).max(0);
}

pub(crate) fn playlist_deleted(id: &str) {
    let mut deleted = DELETED_PLAYLISTS.lock().unwrap();
    deleted.insert(id.to_string());
    let mut library = LIBRARY.lock().unwrap();
    if let Some(data) = library.as_mut() {
        exclude_deleted_playlists(data, &deleted);
    }
    PLAYLIST_DELETION_REVISION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// The RAW local rows behind the feed's `source: "local" | "plex"` tracks,
/// keyed by the same row id the feed row carries.
///
/// It exists because a feed row cannot be turned into a playable local queue
/// track: `local_playback::local_queue_track` needs the file path, the Plex
/// rating key, the CUE offsets and the real source discriminator, and a
/// `FeedItem` carries none of those. The Tracks tab solves the identical
/// problem the identical way (`local_state`'s `tracks_raw`) — this is that
/// cache for the Library "All" feed.
///
/// Refilled wholesale by every `load_library`, so it can never outlive the
/// feed it belongs to. LOCK ORDER: never taken while `LIBRARY` is being
/// written; `feed_track_to_queue` reads it under `LIBRARY`, so writes here
/// always happen first and separately.
static LOCAL_RAW: std::sync::LazyLock<Mutex<HashMap<i64, qbz_library::LocalTrack>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn rank(i: usize, n: usize) -> f32 {
    if n <= 1 {
        0.0
    } else {
        i as f32 / n as f32
    }
}

// ---------------------------------------------------------------------------
// Local favorites store (LocalFavoritesService, ADR-006 backend)
// ---------------------------------------------------------------------------

/// The active user's local-favourites store.
///
/// This cannot be a `OnceLock<LocalFavoritesService>`: logout/login in the
/// same process must replace the SQLite connection, otherwise account B keeps
/// reading and writing account A's database.
static LOCAL_FAVS: LazyLock<Mutex<Option<LocalFavoritesService>>> =
    LazyLock::new(|| Mutex::new(None));

/// Open the per-user local-favorites DB. Called on every session
/// activation (login / restore / offline entry) next to
/// `offline_fwd::init_for_user`.
pub fn init_local_favorites(base_dir: &std::path::Path) {
    // Close the outgoing connection before opening the replacement. If the
    // new store cannot be opened, fail closed to "no local favourites" rather
    // than retaining another account's service.
    if let Ok(mut guard) = LOCAL_FAVS.lock() {
        *guard = None;
    }
    let path = base_dir.join(DB_FILE_NAME);
    match LocalFavoritesService::new(&path) {
        Ok(service) => {
            if let Ok(mut guard) = LOCAL_FAVS.lock() {
                *guard = Some(service);
            }
            log::info!("[qbz-qt] local favorites store opened");
        }
        Err(e) => log::error!("[qbz-qt] local favorites store open failed: {e}"),
    }
}

fn local_favorites_list() -> Vec<qbz_app::settings::local_favorites::LocalFavItem> {
    LOCAL_FAVS
        .lock()
        .ok()
        .and_then(|service| service.as_ref().and_then(|s| s.list().ok()))
        .unwrap_or_default()
}

/// O(1) heart lookup shared by the Local Library row mappers. Keeping this
/// against the same service as the mixed Library feed means a heart never
/// disagrees between those two surfaces after navigation or restart.
pub(crate) fn is_local_favorite(kind: &str, id: &str) -> bool {
    LOCAL_FAVS
        .lock()
        .ok()
        .and_then(|service| {
            service
                .as_ref()
                .map(|service| service.is_favorite(kind, id))
        })
        .unwrap_or(false)
}

/// Toggle a fully described Local Library item. Unlike [`toggle_local`], this
/// does not try to rediscover the snapshot in the mixed Library feed: the
/// Local Library card itself is authoritative for logical album ids that the
/// feed may not currently contain.
pub(crate) fn toggle_local_favorite_snapshot(item: LocalFavItem) -> Option<bool> {
    let service = LOCAL_FAVS.lock().ok()?;
    let service = service.as_ref()?;
    let current = service.is_favorite(&item.kind, &item.id);
    if current {
        if let Err(error) = service.unfavorite(&item.kind, &item.id) {
            log::error!(
                "[qbz-qt] local unfavorite ({}:{}) failed: {error}",
                item.kind,
                item.id
            );
            return Some(current);
        }
        Some(false)
    } else {
        if let Err(error) = service.favorite(&item) {
            log::error!(
                "[qbz-qt] local favorite ({}:{}) failed: {error}",
                item.kind,
                item.id
            );
            return Some(current);
        }
        Some(true)
    }
}

// ---------------------------------------------------------------------------
// Fetch
// ---------------------------------------------------------------------------

/// Page one favorites type until exhausted (favorites.rs loop).
async fn fetch_favorites(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    key: &str,
) -> Result<(Vec<serde_json::Value>, usize), String> {
    collect_favorite_pages(key, |offset| async move {
        runtime
            .core()
            .get_favorites(key, PAGE_SIZE, offset)
            .await
            .map_err(|e| e.to_string())
    })
    .await
}

async fn collect_favorite_pages<F, Fut>(
    key: &str,
    mut fetch: F,
) -> Result<(Vec<serde_json::Value>, usize), String>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<serde_json::Value, String>>,
{
    let mut total: usize;
    let mut all_items: Vec<serde_json::Value> = Vec::new();
    let mut offset = 0u32;
    loop {
        let value = fetch(offset).await?;
        let branch = value.get(key);
        total = branch
            .and_then(|b| b.get("total"))
            .and_then(|t| t.as_u64())
            .unwrap_or(0) as usize;
        let page: Vec<serde_json::Value> = branch
            .and_then(|b| b.get("items"))
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default();
        let page_len = page.len();
        // A server ignoring offset must not turn an uncapped fetch into an
        // endless loop. Fail explicitly instead of publishing truncated data.
        if page_len > 0
            && all_items.len() >= page_len
            && all_items[all_items.len() - page_len..] == page
        {
            return Err(format!(
                "Favorites {key}: server repeated a page at offset {offset}"
            ));
        }
        all_items.extend(page);
        offset = offset
            .checked_add(
                u32::try_from(page_len).map_err(|_| "Favorites page is too large".to_string())?,
            )
            .ok_or_else(|| "Favorites offset overflow".to_string())?;
        if page_len < PAGE_SIZE as usize || (total > 0 && offset as usize >= total) {
            break;
        }
    }
    Ok((all_items, total))
}

/// Refresh the favourite-id cache from the network — the online half of the
/// disk-first lifecycle (`fav_cache_qt::init_for_user` already seeded it at
/// session activation). Fired once per shell entry from `main::enter_shell`,
/// which is where the reference warms the same cache
/// (`crates/qbz/src/main.rs:418-500`, four `tokio::spawn`s next to the
/// sidebar load). Skipped offline: there the disk seed IS the truth.
///
/// Reuses `fetch_favorites` — the same paged reader the Library load uses, so
/// the paging/cap semantics cannot drift between the two. Each type is
/// independent: one failing (labels 400'd for months) must not cost the
/// other three.
pub async fn warm_favorites_cache(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let t = Instant::now();
    // Album ids are TEXT (Qobuz mixes numeric and alphanumeric catalog ids);
    // tracks / artists / labels are numeric.
    match fetch_favorites(runtime, "albums").await {
        Ok((items, _)) => {
            let ids: std::collections::HashSet<String> = items
                .iter()
                .filter_map(|it| it.get("id"))
                .filter_map(|v| match v {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .collect();
            let n = ids.len();
            let _ =
                tokio::task::spawn_blocking(move || crate::fav_cache_qt::set_all_albums(ids)).await;
            log::info!("[qbz-qt] favorites cache warmed: {n} albums");
        }
        Err(e) => log::warn!("[qbz-qt] favorites cache album warm failed: {e}"),
    }
    warm_numeric(runtime, "tracks", crate::fav_cache_qt::set_all_tracks).await;
    warm_numeric(runtime, "artists", crate::fav_cache_qt::set_all_artists).await;
    // PLURAL here, singular on `/favorite/create|delete` — see the
    // `toggle_favorite` note. This is the `type` param's spelling.
    warm_numeric(runtime, "labels", crate::fav_cache_qt::set_all_labels).await;
    log::info!("[qbz-qt][perf] favorites cache warm: {:?}", t.elapsed());
}

/// One numeric favourites type -> its cache setter. `apply` is a plain fn
/// pointer so the blocking disk write can move onto a blocking hop.
async fn warm_numeric(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    key: &str,
    apply: fn(std::collections::HashSet<u64>),
) {
    match fetch_favorites(runtime, key).await {
        Ok((items, _)) => {
            let ids: std::collections::HashSet<u64> = items
                .iter()
                .filter_map(|it| it.get("id").and_then(|v| v.as_u64()))
                .collect();
            let n = ids.len();
            let _ = tokio::task::spawn_blocking(move || apply(ids)).await;
            log::info!("[qbz-qt] favorites cache warmed: {n} {key}");
        }
        Err(e) => log::warn!("[qbz-qt] favorites cache {key} warm failed: {e}"),
    }
}

fn parse_items<T: serde::de::DeserializeOwned>(
    items: Vec<serde_json::Value>,
    what: &str,
) -> Vec<T> {
    items
        .into_iter()
        .filter_map(|v| match serde_json::from_value::<T>(v) {
            Ok(item) => Some(item),
            Err(e) => {
                log::warn!("[qbz-qt] dropping malformed {what}: {e}");
                None
            }
        })
        .collect()
}

fn mmss(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

// ---- mappers (favorites.rs / album_map.rs / search.rs ports) --------------

fn map_track(track: Track) -> FeedItem {
    // §5.1 via §3.1's single interpreter of absence, read HERE and not down in
    // the literal: `is_streamable()` borrows the whole `Track`, and the very
    // next line moves a field out of it. A favourited track Qobuz later pulled
    // is one of the most likely places a user meets this — they hearted it
    // while it was still there.
    let not_streamable = !track.is_streamable();
    let track_id = track.id.to_string();
    let isrc = track.isrc.clone().unwrap_or_default();
    let duration_secs = track.duration;
    let cache_status = if crate::offline_qt::is_cached(&track_id) {
        3
    } else {
        0
    };
    let mut title = track.title;
    if let Some(version) = track.version.as_ref().filter(|v| !v.is_empty()) {
        title = format!("{title} ({version})");
    }
    // Track row art: full variant (best()) — the thumbnail down-tier was reverted after the 2026-08-15 owner smoke (contract 04 §3).
    let artwork_url = track
        .album
        .as_ref()
        .and_then(|a| a.image.best().cloned())
        .unwrap_or_default();
    let album = track
        .album
        .as_ref()
        .map(|a| crate::album_qt::format_album_title(&a.title, a.version.as_deref()))
        .unwrap_or_default();
    let album_id = track
        .album
        .as_ref()
        .map(|a| a.id.clone())
        .unwrap_or_default();
    let genre = track
        .album
        .as_ref()
        .and_then(|a| a.genre.as_ref())
        .map(|g| g.name.clone())
        .unwrap_or_default();
    let album_artist_id = track
        .album
        .as_ref()
        .and_then(|a| a.artist.as_ref())
        .map(|x| x.id.to_string())
        .unwrap_or_default();
    let album_artist = track
        .album
        .as_ref()
        .and_then(|a| a.artist.as_ref())
        .map(|x| x.name.clone())
        .unwrap_or_default();
    let (artist, artist_id) = track
        .performer
        .map(|p| (p.name, p.id.to_string()))
        .unwrap_or_default();
    let label = track
        .album
        .as_ref()
        .and_then(|a| a.label.as_ref())
        .map(|label| label.name.clone())
        .unwrap_or_default();
    let release_sort_key = qbz_text_utils::dates::release_sort_key(
        track.album.as_ref().and_then(|a| a.release_date_original.as_deref()),
    );
    FeedItem {
        kind: "track".into(),
        group: "favorites".into(),
        source: "qobuz".into(),
        id: track_id,
        title,
        subtitle: artist.clone(),
        artist,
        artist_id,
        album,
        album_id,
        album_artist,
        album_artist_id,
        genre,
        label,
        release_sort_key,
        duration: mmss(duration_secs),
        isrc,
        duration_secs,
        quality_tier: home_qt::quality_tier_from_depth(track.maximum_bit_depth).to_string(),
        quality_detail: home_qt::quality_detail_from_parts(
            track.maximum_bit_depth,
            track.maximum_sampling_rate,
        ),
        // See `FeedItem::bit_depth` — the raw catalog numbers ride with the
        // row so the two feed -> queue builders can hand them to the queue.
        bit_depth: track.maximum_bit_depth,
        sample_rate: track.maximum_sampling_rate,
        explicit: track.parental_warning,
        image_url: artwork_url,
        is_favorite: true,
        not_streamable,
        cache_status,
        ..Default::default()
    }
    .keyed()
}

fn map_album(album: Album, ready_offline_tracks: usize) -> FeedItem {
    // Same absence contract as Track: only an explicit false is a catalog
    // withdrawal. A COMPLETE offline copy remains live because playAlbum has
    // an index-backed fallback when /album/get no longer resolves this id.
    let not_streamable = !album.is_streamable();
    let track_count = album.tracks_count.or(album.track_count).unwrap_or(0) as usize;
    let fully_cached = track_count > 0 && ready_offline_tracks >= track_count;
    let bit_depth = album
        .audio_info
        .as_ref()
        .and_then(|a| a.maximum_bit_depth)
        .or(album.maximum_bit_depth);
    let sample_rate = album
        .audio_info
        .as_ref()
        .and_then(|a| a.maximum_sampling_rate)
        .or(album.maximum_sampling_rate);
    let date = home_qt::album_release_date(&album);
    let artist = if !album.artist.name.is_empty() {
        album.artist.name
    } else {
        album
            .artists
            .as_ref()
            .and_then(|c| c.first().map(|a| a.name.clone()))
            .unwrap_or_default()
    };
    let is_pinned = crate::sidebar_qt::is_pinned("album", &album.id);
    // Prefer the user's custom cover override (add_custom_cover) when set, so
    // the Library grid card matches the album page. Borrowed before `album.id`
    // and `album.image` are moved into the struct below.
    let cover_url = crate::cover_artwork_qt::prefer_album_cover(
        &album.id,
        album.image.best().cloned().unwrap_or_default(),
    );
    FeedItem {
        is_pinned,
        kind: "album".into(),
        group: "favorites".into(),
        source: "qobuz".into(),
        id: album.id,
        title: crate::album_qt::format_album_title(&album.title, album.version.as_deref()),
        subtitle: artist.clone(),
        artist,
        artist_id: album.artist.id.to_string(),
        label: album.label.map(|label| label.name).unwrap_or_default(),
        duration_secs: album.duration.unwrap_or(0),
        track_count: Some(track_count as u32),
        genre: album.genre.map(|g| g.name).unwrap_or_default(),
        year: qbz_text_utils::dates::release_label(date.as_deref()),
        release_sort_key: qbz_text_utils::dates::release_sort_key(date.as_deref()),
        quality_tier: home_qt::quality_tier_from_depth(bit_depth).to_string(),
        quality_detail: home_qt::quality_detail_from_parts(bit_depth, sample_rate),
        // Library grid card: full variant (best()) — the down-tier was
        // reverted after the 2026-08-15 owner smoke (contract 04 §3). Custom
        // cover override applied above.
        image_url: cover_url,
        is_favorite: true,
        not_streamable,
        cache_status: if fully_cached { 3 } else { 0 },
        ..Default::default()
    }
    .keyed()
}

fn map_artist(artist: Artist) -> FeedItem {
    // Prefer the user's custom portrait (add_custom_image) when set — keyed by
    // artist NAME — so the card matches the artist page. Borrowed before
    // `artist.name`/`artist.image` are moved into the struct.
    let image_url = crate::cover_artwork_qt::prefer_artist_image(
        &artist.name,
        artist
            .image
            .as_ref()
            .and_then(|img| img.best().cloned())
            .unwrap_or_default(),
    );
    FeedItem {
        // Pin badge state from the per-user store. The album mapper above
        // has always seeded it; the artist and playlist rows did not, so
        // every artist/playlist card in the Library grid drew the hollow
        // glyph and the first click un-pinned what the user meant to pin.
        is_pinned: crate::sidebar_qt::is_pinned("artist", &artist.id.to_string()),
        kind: "artist".into(),
        group: "following".into(),
        source: "qobuz".into(),
        id: artist.id.to_string(),
        title: artist.name,
        image_url,
        is_favorite: true,
        ..Default::default()
    }
    .keyed()
}

/// Up to four de-duplicated member-track covers (`images300` > `images150`
/// > `images`) — the collage source. Same picker as `sidebar_qt::load` and
/// `qbz::search::playlist_cover_urls`; these lists are the covers of the
/// playlist's MEMBER ALBUMS, never the playlist's own artwork.
///
/// `pub(crate)` since the follow seam landed: `playlist_qt::FollowRowMeta`
/// synthesizes a Library row from the API model and must pick its covers with
/// THIS function, not a second copy of the precedence — two copies of it is
/// how the cards ended up showing a member-album sleeve where the playlist
/// graphic belongs.
pub(crate) fn playlist_cover_urls(playlist: &Playlist) -> Vec<String> {
    // A custom playlist cover replaces the mosaic everywhere (one tile).
    if let Some(p) = crate::cover_artwork_qt::playlist_cover(&playlist.id.to_string()) {
        if std::path::Path::new(&p).is_file() {
            return vec![p];
        }
    }
    let source = [&playlist.images300, &playlist.images150, &playlist.images]
        .into_iter()
        .flatten()
        .find(|v| !v.is_empty());
    let mut out: Vec<String> = Vec::new();
    if let Some(list) = source {
        for url in list {
            if url.is_empty() || out.contains(url) {
                continue;
            }
            out.push(url.clone());
            if out.len() == 4 {
                break;
            }
        }
    }
    out
}

/// The playlist's OWN Qobuz artwork: the editorial `image_rectangle`
/// graphic (`image_rectangle_mini` as the lighter fallback). Only Qobuz
/// editorial playlists carry one — user-created playlists have none and
/// fall back to the member-cover collage, exactly the Tauri split between
/// `QobuzPlaylistCard` (`image.rectangle`, contain-fitted) and
/// `FavoritePlaylistCard` (`PlaylistCollage`).
///
/// `pub(crate)` for the same reason as [`playlist_cover_urls`].
pub(crate) fn playlist_own_image(playlist: &Playlist) -> String {
    [&playlist.image_rectangle, &playlist.image_rectangle_mini]
        .into_iter()
        .flatten()
        .find_map(|list| list.iter().find(|u| !u.is_empty()).cloned())
        .unwrap_or_default()
}

/// The playlist card's subtitle line: the owner, then the track count.
///
/// Factored out of [`map_playlist_row`] so the follow / copy seams
/// ([`insert_playlist_row`]) can synthesize a row that reads exactly like the
/// one the next full load will produce — the alternative was a second copy of
/// this format string drifting from this one.
pub(crate) fn playlist_subtitle(owner: &str, tracks_count: u32) -> String {
    let mut subtitle = owner.to_string();
    if tracks_count > 0 {
        let tracks_label = qbz_i18n::tf(
            "{} track",
            "{} tracks",
            tracks_count as i64,
            &[&tracks_count.to_string()],
        );
        subtitle = if subtitle.is_empty() {
            tracks_label
        } else {
            format!("{}   •   {}", subtitle, tracks_label)
        };
    }
    subtitle
}

fn playlist_creator(owned: bool, owner_name: &str) -> &'static str {
    if owned {
        "you"
    } else if owner_name
        .split_whitespace()
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("qobuz"))
    {
        "qobuz"
    } else if owner_name.trim().is_empty() {
        "unknown"
    } else {
        "others"
    }
}

fn map_playlist_row(playlist: &Playlist, is_following: bool) -> FeedItem {
    // The card's single image is the playlist's own Qobuz artwork ONLY.
    // Falling back to `images300[0]` here is what put a member ALBUM cover
    // on every playlist card; the mosaic is fed separately via `covers`.
    let cover_url = playlist_own_image(playlist);
    let covers = playlist_cover_urls(playlist);
    let subtitle = playlist_subtitle(&playlist.owner.name, playlist.tracks_count);
    let uid = UserDataPaths::load_last_user_id();
    let owned = uid.map(|uid| uid == playlist.owner.id).unwrap_or(false);
    FeedItem {
        // Pin badge state from the per-user store (see `map_artist`).
        is_pinned: crate::sidebar_qt::is_pinned("playlist", &playlist.id.to_string()),
        kind: "playlist".into(),
        group: if is_following {
            "following"
        } else {
            "favorites"
        }
        .into(),
        source: "qobuz".into(),
        id: playlist.id.to_string(),
        title: playlist.name.clone(),
        subtitle,
        track_count: Some(playlist.tracks_count),
        duration_secs: playlist.duration,
        updated_at: playlist.updated_at,
        playlist_own_image: !cover_url.is_empty(),
        image_url: cover_url,
        covers,
        // The library.db heart, NOT "this row landed in the favorites group".
        // `!is_following` answers a different question — which BUCKET the row
        // was fetched into — and the cards render this field as the heart while
        // `toggle_playlist_favorite` takes its direction from library.db. With
        // the two conflated, every OWNED playlist drew a filled heart reading
        // "Remove from Library" even when it had never been hearted, and the
        // click wrote `false` over a flag that was already false. The bucket is
        // still available to callers as `group` / `playlist_following`.
        is_favorite: crate::fav_cache_qt::is_favorite("playlist", &playlist.id.to_string()),
        playlist_owned: owned,
        playlist_creator: playlist_creator(owned, &playlist.owner.name).into(),
        playlist_following: is_following,
        ..Default::default()
    }
    .keyed()
}

/// Ready offline rows grouped by Qobuz album id.
///
/// Library can contain hundreds of favorite albums, so snapshot the index in
/// one pass rather than issuing a query per card. The count is compared with
/// the catalog track count in `map_album`; a partial download must not make a
/// withdrawn album look wholly playable.
async fn ready_offline_album_track_counts() -> std::collections::HashMap<String, usize> {
    use qbz_offline_cache::OfflineCacheStatus;

    let Some(offline) = crate::offline_qt::get().await else {
        return std::collections::HashMap::new();
    };
    let tracks = {
        let db = offline.db.lock().await;
        db.as_ref()
            .and_then(|db| db.get_all_tracks().ok())
            .unwrap_or_default()
    };
    let mut counts = std::collections::HashMap::new();
    for track in tracks {
        if track.status != OfflineCacheStatus::Ready {
            continue;
        }
        if let Some(album_id) = track.album_id.filter(|id| !id.is_empty()) {
            *counts.entry(album_id).or_insert(0) += 1;
        }
    }
    counts
}

/// Three-way release verdict. `Unknown` is deliberately not a bool: collapsing
/// a terse embedded album or a failed probe into `Unavailable` would hide a
/// valid Go-to-album action and expose a destructive bulk action on a network
/// failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReleaseAvailability {
    Available,
    Unavailable,
    Unknown,
}

fn embedded_release_availability(track: &Track) -> ReleaseAvailability {
    let Some(album) = track.album.as_ref() else {
        return ReleaseAvailability::Unknown;
    };
    match album.streamable {
        Some(true) => ReleaseAvailability::Available,
        Some(false) if album.is_upcoming() => ReleaseAvailability::Available,
        Some(false) => ReleaseAvailability::Unavailable,
        None => ReleaseAvailability::Unknown,
    }
}

/// A successful `/album/get` can prove a release-wide withdrawal either at
/// the album level or through a non-empty track list in which every row is an
/// explicit, non-upcoming withdrawal. Absence is never proof.
fn fetched_release_unavailable(album: &Album) -> bool {
    if qbz_models::types::upcoming_now(None, album.release_date_stream.as_deref()) {
        return false;
    }
    if album.streamable == Some(false) {
        return true;
    }
    album
        .tracks
        .as_ref()
        .map(|tracks| {
            !tracks.items.is_empty()
                && tracks
                    .items
                    .iter()
                    .all(|track| track.streamable == Some(false) && !track.is_upcoming())
        })
        .unwrap_or(false)
}

async fn probe_release_availability(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    album_id: String,
) -> (String, ReleaseAvailability) {
    let verdict = match runtime.core().get_album(&album_id).await {
        Ok(album) if fetched_release_unavailable(&album) => ReleaseAvailability::Unavailable,
        Ok(_) => ReleaseAvailability::Available,
        Err(qbz_core::CoreError::Api(error)) if error.is_album_unavailable(&album_id) => {
            ReleaseAvailability::Unavailable
        }
        Err(error) => {
            log::warn!(
                "[qbz-qt] library release availability probe {album_id} failed; keeping it unknown: {error}"
            );
            ReleaseAvailability::Unknown
        }
    };
    (album_id, verdict)
}

/// Resolve release-wide availability only for favourite tracks already known
/// to be unavailable. Explicit embedded answers cost no request; unknown album
/// ids are de-duplicated and probed with a bounded fan-out.
async fn unavailable_release_ids(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    tracks: &[Track],
) -> HashSet<String> {
    let mut verdicts: HashMap<String, ReleaseAvailability> = HashMap::new();
    for track in tracks.iter().filter(|track| !track.is_streamable()) {
        let Some(album) = track.album.as_ref().filter(|album| !album.id.is_empty()) else {
            continue;
        };
        let incoming = embedded_release_availability(track);
        verdicts
            .entry(album.id.clone())
            .and_modify(|current| {
                // A positive availability signal wins over a contradictory
                // tombstone signal; refusing valid navigation is the less
                // recoverable mistake. Otherwise preserve unavailable over
                // unknown.
                *current = match (*current, incoming) {
                    (ReleaseAvailability::Available, _) | (_, ReleaseAvailability::Available) => {
                        ReleaseAvailability::Available
                    }
                    (ReleaseAvailability::Unavailable, _)
                    | (_, ReleaseAvailability::Unavailable) => ReleaseAvailability::Unavailable,
                    _ => ReleaseAvailability::Unknown,
                }
            })
            .or_insert(incoming);
    }

    let to_probe: Vec<String> = verdicts
        .iter()
        .filter(|(_, verdict)| **verdict == ReleaseAvailability::Unknown)
        .map(|(id, _)| id.clone())
        .collect();
    let probed = stream::iter(to_probe)
        .map(|album_id| probe_release_availability(runtime, album_id))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    for (id, verdict) in probed {
        verdicts.insert(id, verdict);
    }

    verdicts
        .into_iter()
        .filter_map(|(id, verdict)| (verdict == ReleaseAvailability::Unavailable).then_some(id))
        .collect()
}

/// The whole library load: fan out, normalize, merge (library_all.rs
/// ordering semantics), compute the tab counts.
pub async fn load_library(runtime: &Arc<AppRuntime<LoggingAdapter>>) -> Result<usize, String> {
    let t_fetch = Instant::now();

    // Favorites per type (paged) — sequential shares one client; measured
    // individually for the perf report.
    let t = Instant::now();
    let (raw_tracks, tracks_total) = fetch_favorites(runtime, "tracks").await?;
    log::info!(
        "[qbz-qt][perf] favorites tracks fetch: {:?} ({} items)",
        t.elapsed(),
        raw_tracks.len()
    );
    let t = Instant::now();
    let (raw_albums, albums_total) = fetch_favorites(runtime, "albums").await?;
    log::info!(
        "[qbz-qt][perf] favorites albums fetch: {:?} ({} items)",
        t.elapsed(),
        raw_albums.len()
    );
    let t = Instant::now();
    let (raw_artists, artists_total) = fetch_favorites(runtime, "artists").await?;
    log::info!(
        "[qbz-qt][perf] favorites artists fetch: {:?} ({} items)",
        t.elapsed(),
        raw_artists.len()
    );
    let t = Instant::now();
    let (raw_labels, labels_total) = fetch_favorites(runtime, "labels").await?;
    log::info!(
        "[qbz-qt][perf] favorites labels fetch: {:?} ({} items)",
        t.elapsed(),
        raw_labels.len()
    );

    // Playlists (owned + followed + locally hearted).
    let t = Instant::now();
    let all_playlists = runtime
        .core()
        .get_user_playlists()
        .await
        .map_err(|e| e.to_string())?;
    // Refresh the playlist ownership / follow snapshot from this same
    // response (see `playlist_qt::set_user_playlists`) — the Library load is
    // the second of the two places that fetch the user's playlist list, and
    // the fresher of the two.
    {
        let pairs: Vec<(u64, u64)> = all_playlists.iter().map(|p| (p.id, p.owner.id)).collect();
        crate::playlist_qt::set_user_playlists(&pairs);
    }
    let uid = UserDataPaths::load_last_user_id();
    let following: Vec<FeedItem> = match uid {
        Some(uid) => all_playlists
            .iter()
            .filter(|p| p.owner.id != uid)
            .map(|p| map_playlist_row(p, true))
            .collect(),
        None => Vec::new(),
    };
    let mut pl_favorites: Vec<FeedItem> = Vec::new();
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
    if let Some(uid) = uid {
        for p in all_playlists.iter().filter(|p| p.owner.id == uid) {
            seen.insert(p.id);
            pl_favorites.push(map_playlist_row(p, false));
        }
    }
    // Hearted playlist ids from the per-user library.db (favorites.rs). This
    // is the SAME set `fav_cache_qt` mirrors for the card producers, so
    // refresh the mirror while it is in hand — the session seed was taken at
    // activation and a heart set from another surface since then would
    // otherwise only reach it through that surface's own write-through.
    let mut fav_pl_ids = crate::library_db_qt::favorite_playlist_ids();
    with_deleted_playlists(|deleted| {
        fav_pl_ids.retain(|id| !deleted.contains(&id.to_string()));
        crate::fav_cache_qt::set_all_playlists(fav_pl_ids.iter().copied().collect());
    });
    for fid in fav_pl_ids {
        if !seen.insert(fid) {
            continue;
        }
        if let Some(p) = all_playlists.iter().find(|p| p.id == fid) {
            pl_favorites.push(map_playlist_row(p, p.owner.id != uid.unwrap_or(0)));
        } else if let Ok(p) = runtime.core().get_playlist(fid).await {
            pl_favorites.push(map_playlist_row(&p, false));
        }
    }
    let playlists_total = pl_favorites.len() as i64;
    log::info!(
        "[qbz-qt][perf] playlists fetch: {:?} ({} fav / {} following)",
        t.elapsed(),
        pl_favorites.len(),
        following.len()
    );

    // Purchases (group "purchases") — best-effort: a purchase-less account
    // returns empty; a failure must not sink the library.
    let t = Instant::now();
    let (purchase_albums, purchase_tracks) = fetch_purchases(runtime).await;
    log::info!(
        "[qbz-qt][perf] purchases fetch: {:?} ({} albums / {} tracks)",
        t.elapsed(),
        purchase_albums.len(),
        purchase_tracks.len()
    );

    log::info!("[qbz-qt][perf] total fetch wall: {:?}", t_fetch.elapsed());

    // ---- Map + merge ------------------------------------------------------
    let t_map = Instant::now();
    let mut feed: Vec<FeedItem> = Vec::new();

    let tracks: Vec<Track> = parse_items(raw_tracks, "track");
    let unavailable_releases = unavailable_release_ids(runtime, &tracks).await;
    let n = tracks.len();
    for (i, track) in tracks.into_iter().enumerate() {
        let mut item = map_track(track);
        item.release_unavailable = unavailable_releases.contains(&item.album_id);
        item.added_rank = rank(i, n);
        feed.push(item);
    }
    let albums: Vec<Album> = parse_items(raw_albums, "album");
    let ready_album_tracks = ready_offline_album_track_counts().await;
    let n = albums.len();
    for (i, album) in albums.into_iter().enumerate() {
        let ready = ready_album_tracks.get(&album.id).copied().unwrap_or(0);
        let mut item = map_album(album, ready);
        item.added_rank = rank(i, n);
        feed.push(item);
    }
    let artists: Vec<Artist> = parse_items(raw_artists, "artist");
    let n = artists.len();
    for (i, item) in artists.into_iter().map(map_artist).enumerate() {
        let mut item = item;
        item.added_rank = rank(i, n);
        feed.push(item);
    }
    let n = pl_favorites.len();
    for (i, item) in pl_favorites.into_iter().enumerate() {
        let mut item = item;
        item.added_rank = rank(i, n);
        feed.push(item);
    }
    let n = following.len();
    for (i, item) in following.into_iter().enumerate() {
        let mut item = item;
        item.added_rank = rank(i, n);
        feed.push(item);
    }
    let n = purchase_albums.len();
    for (i, mut item) in purchase_albums.into_iter().enumerate() {
        item.added_rank = rank(i, n);
        feed.push(item);
    }
    let n = purchase_tracks.len();
    for (i, mut item) in purchase_tracks.into_iter().enumerate() {
        item.added_rank = rank(i, n);
        feed.push(item);
    }

    // Labels (no card artwork fields beyond the image + "{n} albums" line).
    /// favorites.rs-local richer label shape (image + count).
    #[derive(serde::Deserialize)]
    struct FavLabel {
        #[serde(default)]
        id: u64,
        #[serde(default)]
        name: String,
        #[serde(default)]
        albums_count: Option<u32>,
        #[serde(default)]
        image: Option<serde_json::Value>,
    }
    let labels: Vec<FavLabel> = parse_items(raw_labels, "label");
    let n = labels.len();
    for (i, l) in labels.into_iter().enumerate() {
        feed.push(
            FeedItem {
                kind: "label".into(),
                group: "following".into(),
                source: "qobuz".into(),
                subtitle: match l.albums_count {
                    Some(c) if c > 0 => format!("{c} albums"),
                    _ => String::new(),
                },
                id: l.id.to_string(),
                title: l.name,
                image_url: extract_label_image(l.image.as_ref()),
                is_favorite: true,
                added_rank: rank(i, n),
                ..Default::default()
            }
            .keyed(),
        );
    }

    // Local + Plex layer (show-local default ON; it bypasses the three Qobuz
    // source switches). The Settings scope picks the CONTENT: "favorites" =
    // hearted items only (webplayer parity), "all" = the entire local library.
    // library_all.rs:300-331.
    if crate::library_prefs::local_scope() == "all" {
        match tokio::task::spawn_blocking(all_local_feed_blocking).await {
            Ok(items) => feed.extend(items),
            Err(e) => log::error!("[qbz-qt] all-local feed load failed: {e}"),
        }
    } else {
        // The raw-row cache belongs to the "all" branch; a scope flipped back
        // to "favorites" must not leave the previous build's rows behind for
        // `feed_track_to_queue` to resolve against.
        if let Ok(mut raw) = LOCAL_RAW.lock() {
            raw.clear();
        }
        let locals = local_favorites_list();
        let album_candidates = locals
            .iter()
            .filter(|item| item.kind == "album")
            .map(|item| (item.id.clone(), item.source.clone()))
            .collect::<Vec<_>>();
        // A failed availability query must not turn every favorite into a
        // tombstone.  `None` means "unknown" and leaves rows interactive;
        // the next successful Library load will settle them.
        let existing_albums: Option<HashMap<String, Vec<String>>> = if album_candidates.is_empty() {
            Some(HashMap::new())
        } else {
            match tokio::task::spawn_blocking(move || {
                crate::local_albums::existing_favorite_album_sources_blocking(album_candidates)
            })
            .await
            {
                Ok(Ok(ids)) => Some(ids),
                Ok(Err(error)) => {
                    log::warn!("[qbz-qt] local favorite availability check failed: {error}");
                    None
                }
                Err(error) => {
                    log::warn!("[qbz-qt] local favorite availability worker failed: {error}");
                    None
                }
            }
        };
        let n = locals.len();
        for (i, lf) in locals.into_iter().enumerate() {
            let source_unavailable = lf.kind == "album"
                && existing_albums
                    .as_ref()
                    .is_some_and(|ids| !ids.contains_key(&lf.id));
            let sources = existing_albums
                .as_ref()
                .and_then(|albums| albums.get(&lf.id))
                .cloned()
                .unwrap_or_default();
            feed.push(
                FeedItem {
                    kind: lf.kind,
                    group: "local".into(),
                    source: lf.source,
                    sources,
                    subtitle: lf.subtitle,
                    artist: lf.artist.clone(),
                    image_url: lf.artwork_url,
                    is_favorite: true,
                    source_unavailable,
                    added_rank: rank(i, n),
                    id: lf.id,
                    title: lf.title,
                    ..Default::default()
                }
                .keyed(),
            );
        }
    }

    // Merge by recency proxy (stable — equal ranks keep source order).
    feed.sort_by(|a, b| {
        a.added_rank
            .partial_cmp(&b.added_rank)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let all_total = feed.len() as i64;
    log::info!(
        "[qbz-qt][perf] map+merge: {:?} ({} feed items)",
        t_map.elapsed(),
        feed.len()
    );

    let counts = LibraryCounts {
        tracks: tracks_total as i64,
        albums: albums_total as i64,
        artists: artists_total as i64,
        playlists: playlists_total,
        labels: labels_total as i64,
        all: all_total,
    };
    with_deleted_playlists(|deleted| {
        let mut data = LibraryData { feed, counts };
        exclude_deleted_playlists(&mut data, deleted);
        *LIBRARY.lock().unwrap() = Some(data);
    });
    Ok(all_total as usize)
}

/// The `"all"` local scope: the ENTIRE local library (plus Plex when the
/// integration is on) as feed rows — albums, artists and tracks — instead of
/// just the hearted ones. Port of `crates/qbz/src/library_all.rs:348-516`
/// (`all_local_blocking`), over the very queries the Local Library tabs run,
/// so the two views can never disagree about what is in the library.
///
/// Blocking by construction (rusqlite): the caller wraps it in
/// `spawn_blocking`.
///
/// TWO deliberate deltas from the reference, both because the Qt side already
/// had the better primitive:
///  - the local↔Plex artist merge keys on `local_artist_match::normalize_artist`
///    (lowercase + diacritic fold + punctuation collapse) rather than a raw
///    `trim().to_lowercase()`. That is PARITY-DEBT #7, already fixed for the
///    Artists tab: without it "Sigur Rós" and "Sigur Ros" are two rows.
///  - the quality badge comes from `local_rows::tier_of` +
///    `home_qt::quality_detail_from_parts`, the pair every other local surface
///    in this port uses.
///
/// It also fills [`LOCAL_RAW`], without which none of these track rows would
/// be playable — see that static.
fn all_local_feed_blocking() -> Vec<FeedItem> {
    use crate::local_rows::tier_of;
    use crate::local_state::{group_mode, with_db, TRACKS_PAGE};

    /// `FeedItem::sample_rate` is kHz (it feeds the NPB AudioStamp through
    /// `feed_track_to_queue`), while `qbz_library` stores Hz on both the album
    /// and the track. `local_queue_track` does the identical conversion for the
    /// queue side; skipping it here renders "44100 kHz" on the stamp.
    fn khz(hz: f64) -> f64 {
        if hz >= 1000.0 {
            hz / 1000.0
        } else {
            hz
        }
    }

    let t0 = Instant::now();
    let mut out: Vec<FeedItem> = Vec::new();
    let plex_on = crate::local_plex::is_configured();
    let plex_path = crate::local_plex::cache_db_path();
    // The SHARED remote mirror, and which of its sources may show. Both gates
    // matter: the path short-circuits the ATTACH for a user with no media
    // server, and the words are what make the master toggle actually remove a
    // server's rows from the grid (the mirror holds them all).
    let remote_path = crate::media_servers_qt::remote_cache_path();
    let remote_words = crate::media_servers_qt::configured_words();
    let mode = group_mode();

    // The hearts, once. The reference asks the store per row
    // (`local_favorites::is_favorite`); at "entire local library" scale that
    // is one lock per row, so the key set is snapshotted instead.
    let hearts: std::collections::HashSet<(String, String)> = local_favorites_list()
        .into_iter()
        .map(|f| (f.kind, f.id))
        .collect();
    let hearted = |kind: &str, id: &str| hearts.contains(&(kind.to_string(), id.to_string()));

    // Plex tracks ride TWO sections (the artist names and the track list), so
    // the bounded set is fetched ONCE — the Tracks tab's page-1 merge.
    let plex_tracks: Vec<qbz_library::LocalTrack> = if plex_on {
        crate::local_plex::search_tracks("")
    } else {
        Vec::new()
    };

    // --- Albums: the Albums tab's own full-load page (the query folds the
    // Plex union in when the cache path is passed). ---
    let albums = with_db(|db| {
        db.get_albums_metadata_page(
            0,
            100_000,
            None,
            "artist",
            "asc",
            /* include_qobuz_downloads */ true,
            /* exclude_network_folders */ false,
            plex_path.as_deref(),
            remote_path.as_deref(),
            &remote_words,
            mode,
        )
        .map(|p| p.albums)
    })
    .unwrap_or_default();
    let n = albums.len();
    for (i, a) in albums.into_iter().enumerate() {
        // The row's own `source` word, NOT the id prefix. Both answer "plex"
        // for a Plex album (group_key is `plex:<hash>` and the CTE stamps
        // 'plex'), so this is byte-identical for the two sources that existed
        // — but a media server's group_key is `jellyfin:<id>` / `navidrome:<id>`,
        // and the prefix test folded every one of them to "local". That is the
        // same failure `local_rows::badge_source` had: the ALL feed drew the
        // local hard-drive on albums that live on a server.
        //
        // "user" and "qobuz_download" keep answering "local" exactly as before;
        // this feed's `source` vocabulary is consumed by `feed_track_to_queue`
        // and by the QML badge, and widening it to "offline" here would be a
        // separate change with its own consumers to check.
        let source = match a.source.as_str() {
            "plex" => "plex",
            "jellyfin" => "jellyfin",
            "subsonic" | "navidrome" | "gonic" | "airsonic" | "astiga" => "subsonic",
            _ => "local",
        };
        let sources = crate::local_rows::album_badge_sources(&a);
        out.push(
            FeedItem {
                kind: "album".into(),
                group: "local".into(),
                source: source.into(),
                sources,
                subtitle: a.artist.clone(),
                artist: a.artist,
                image_url: a.artwork_path.unwrap_or_default(),
                quality_tier: tier_of(&a.format, a.bit_depth, a.sample_rate).into(),
                quality_detail: crate::local_rows::detail_of(&a.format, a.bit_depth, a.sample_rate),
                bit_depth: a.bit_depth,
                sample_rate: Some(khz(a.sample_rate)),
                genre: a.genres.join(", "),
                track_count: Some(a.track_count),
                duration_secs: a.total_duration_secs.min(u32::MAX as u64) as u32,
                duration: mmss(a.total_duration_secs.min(u32::MAX as u64) as u32),
                is_favorite: hearted("album", &a.id),
                release_sort_key: a
                    .year
                    .map(|y| qbz_text_utils::dates::release_sort_key(Some(&y.to_string())))
                    .unwrap_or(0),
                year: a.year.map(|y| y.to_string()).unwrap_or_default(),
                added_rank: rank(i, n),
                id: a.id,
                title: a.title,
                ..Default::default()
            }
            .keyed(),
        );
    }

    // --- Artists: the on-disk names, plus names that exist ONLY on Plex
    // tracks. An artist present in both counts once, and the local spelling
    // wins (it is the one with albums behind it). ---
    let local_names: Vec<String> = with_db(|db| {
        db.get_artists_with_filter(
            /* include_qobuz_downloads */ true, /* exclude_network_folders */ false,
        )
    })
    .unwrap_or_default()
    .into_iter()
    .map(|a| a.name)
    .filter(|n| !n.trim().is_empty())
    .collect();
    let local_keys: std::collections::HashSet<String> = local_names
        .iter()
        .map(|n| crate::local_artist_match::normalize_artist(n))
        .collect();
    let mut names = local_names;
    {
        let mut seen = local_keys.clone();
        for t in &plex_tracks {
            let name = t.artist.trim();
            if !name.is_empty() && seen.insert(crate::local_artist_match::normalize_artist(name)) {
                names.push(name.to_string());
            }
        }
    }
    names.sort_by_key(|n| n.to_lowercase());
    let n = names.len();
    for (i, name) in names.into_iter().enumerate() {
        let source = if local_keys.contains(&crate::local_artist_match::normalize_artist(&name)) {
            "local"
        } else {
            "plex"
        };
        out.push(
            FeedItem {
                kind: "artist".into(),
                group: "local".into(),
                source: source.into(),
                is_favorite: hearted("artist", &name),
                added_rank: rank(i, n),
                id: name.clone(),
                title: name,
                ..Default::default()
            }
            .keyed(),
        );
    }

    // --- Tracks: the local pages (the SQL already excludes offline-cache
    // copies) plus the Plex set fetched above. The 200k ceiling is the
    // reference's — a runaway library must not take the feed build with it. ---
    let mut tracks: Vec<qbz_library::LocalTrack> = Vec::new();
    let mut offset = 0u64;
    loop {
        let Some(rows) = with_db(|db| {
            db.search_with_filter_page(
                "",
                offset,
                TRACKS_PAGE,
                /* include_qobuz_downloads */ false,
                /* exclude_network_folders */ false,
                "default",
            )
        }) else {
            break;
        };
        let full = rows.len() as u64 == TRACKS_PAGE;
        tracks.extend(rows);
        if !full || tracks.len() >= 200_000 {
            break;
        }
        offset += TRACKS_PAGE;
    }
    tracks.extend(plex_tracks);

    // The raw rows FIRST: `feed_track_to_queue` resolves against this map, and
    // publishing feed rows that cannot be played is the failure this closes.
    if let Ok(mut raw) = LOCAL_RAW.lock() {
        raw.clear();
        raw.reserve(tracks.len());
        for t in &tracks {
            raw.insert(t.id, t.clone());
        }
    }

    let n = tracks.len();
    for (i, t) in tracks.into_iter().enumerate() {
        let source = match t.source.as_deref() {
            Some("plex") => "plex",
            // Belt: the SQL above already excludes them, and an offline copy
            // is a QOBUZ row wearing a local file — it must not enter the feed
            // as a local one.
            Some("qobuz_download") => continue,
            _ => "local",
        };
        let id = t.id.to_string();
        out.push(
            FeedItem {
                kind: "track".into(),
                group: "local".into(),
                source: source.into(),
                subtitle: t.artist.clone(),
                artist: t.artist,
                album: t.album_group_title,
                album_id: t.album_group_key,
                image_url: t.artwork_path.unwrap_or_default(),
                quality_tier: tier_of(&t.format, t.bit_depth, t.sample_rate).into(),
                quality_detail: crate::local_rows::detail_of(&t.format, t.bit_depth, t.sample_rate),
                bit_depth: t.bit_depth,
                sample_rate: Some(khz(t.sample_rate)),
                is_favorite: hearted("track", &id),
                genre: t.genre.unwrap_or_default(),
                year: t.year.map(|y| y.to_string()).unwrap_or_default(),
                duration: crate::local_rows::mmss(t.duration_secs),
                added_rank: rank(i, n),
                id,
                title: t.title,
                ..Default::default()
            }
            .keyed(),
        );
    }

    log::info!(
        "[qbz-qt][perf] all-local feed: {:?} ({} rows)",
        t0.elapsed(),
        out.len()
    );
    out
}

fn map_purchased_album(a: qbz_models::PurchaseAlbum) -> FeedItem {
    let tier = if a.hires { "hires" } else { "cd" };
    FeedItem {
        // Purchase rows are the ONE group in this feed whose flags are
        // not implied by the group itself: a purchased album can also
        // be pinned and can also be a favourite, and both were left at
        // their `Default` false. The card then drew a hollow heart and
        // an unpinned badge over an album that IS both, and the first
        // click on either REMOVED it. Same two O(1) reads every other
        // producer does.
        is_pinned: crate::sidebar_qt::is_pinned("album", &a.id),
        is_favorite: crate::fav_cache_qt::is_album_favorite(&a.id),
        kind: "album".into(),
        group: "purchases".into(),
        source: "qobuz".into(),
        subtitle: a.artist.name.clone(),
        artist: a.artist.name,
        artist_id: a.artist.id.to_string(),
        image_url: a.image.best().cloned().unwrap_or_default(),
        quality_tier: tier.into(),
        year: qbz_text_utils::dates::release_label(a.release_date_original.as_deref()),
        release_sort_key: qbz_text_utils::dates::release_sort_key(
            a.release_date_original.as_deref(),
        ),
        label: a.label.as_ref().map(|label| label.name.clone()).unwrap_or_default(),
        duration_secs: a.duration.unwrap_or(0),
        track_count: a.tracks_count,
        genre: a.genre.as_ref().map(|g| g.name.clone()).unwrap_or_default(),
        id: a.id,
        title: a.title,
        ..Default::default()
    }
    .keyed()
}

/// Purchases via the offline-cache crate's pass-through (best-effort).
async fn fetch_purchases(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
) -> (Vec<FeedItem>, Vec<FeedItem>) {
    let client_lock = runtime.core().client();
    let client = {
        let guard = client_lock.read().await;
        guard.as_ref().cloned()
    };
    let Some(client) = client else {
        return (Vec::new(), Vec::new());
    };
    let Ok(response) = qbz_offline_cache::purchases_service::get_user_purchases_all(&client).await
    else {
        return (Vec::new(), Vec::new());
    };
    let filtered = qbz_offline_cache::purchases_service::filter_purchase_response(response, "");
    let albums = filtered
        .albums
        .items
        .into_iter()
        .map(map_purchased_album)
        .collect();
    let tracks = filtered
        .tracks
        .items
        .into_iter()
        .map(|t| {
            // Track row art: full variant (best()) — the thumbnail down-tier was reverted after the 2026-08-15 owner smoke (contract 04 §3).
            let (img, alb, aid, album_artist_id) = t
                .album
                .as_ref()
                .map(|a| {
                    (
                        a.image.best().cloned().unwrap_or_default(),
                        a.title.clone(),
                        a.id.clone(),
                        a.artist
                            .as_ref()
                            .map(|x| x.id.to_string())
                            .unwrap_or_default(),
                    )
                })
                .unwrap_or_default();
            let tier = if t.hires { "hires" } else { "cd" };
            FeedItem {
                // Tracks are not pinnable, but they ARE favouritable — see the
                // album arm above for why this cannot stay at Default::false.
                is_favorite: crate::fav_cache_qt::contains_track(t.id),
                kind: "track".into(),
                group: "purchases".into(),
                source: "qobuz".into(),
                subtitle: t.performer.name.clone(),
                artist: t.performer.name,
                artist_id: t.performer.id.to_string(),
                duration: mmss(t.duration),
                duration_secs: t.duration,
                label: t.album.as_ref().and_then(|a| a.label.as_ref())
                    .map(|label| label.name.clone()).unwrap_or_default(),
                genre: t.album.as_ref().and_then(|a| a.genre.as_ref())
                    .map(|genre| genre.name.clone()).unwrap_or_default(),
                release_sort_key: qbz_text_utils::dates::release_sort_key(
                    t.album
                        .as_ref()
                        .and_then(|a| a.release_date_original.as_deref()),
                ),
                album: alb,
                album_id: aid,
                album_artist_id,
                image_url: img,
                quality_tier: tier.into(),
                // See `FeedItem::bit_depth` — a purchased TRACK row is a queue
                // source too (`feed_track_to_queue` / `playback_qt::
                // feed_queue_track` look this row up by id), so it carries the
                // raw catalog numbers like every other track producer.
                bit_depth: t.maximum_bit_depth,
                sample_rate: t.maximum_sampling_rate,
                id: t.id.to_string(),
                title: t.title,
                ..Default::default()
            }
            .keyed()
        })
        .collect();
    (albums, tracks)
}

/// Label image — 1:1 with the reference `label.rs::extract_label_image`.
/// The value is flexible: the favorites wire ships a BARE STRING (Android
/// DTO), `/label/page` ships an object keyed mega/extralarge/large/... .
/// The string arm must come first — `Value::get(key)` on a string is always
/// `None`, so without it every favourite label resolved to "" and the card
/// fell back to the gradient placeholder. The `is_empty` guard is a port
/// addition (the reference lacks it) and cannot regress the key order.
fn extract_label_image(image: Option<&serde_json::Value>) -> String {
    let Some(image) = image else {
        return String::new();
    };
    if let Some(url) = image.as_str() {
        return url.to_string();
    }
    for key in ["mega", "extralarge", "large", "thumbnail", "small"] {
        if let Some(url) = image.get(key).and_then(|v| v.as_str()) {
            if !url.is_empty() {
                return url.to_string();
            }
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Accessors for the bridge / favorite toggles
// ---------------------------------------------------------------------------

pub fn with_library<R>(f: impl FnOnce(&LibraryData) -> R) -> Option<R> {
    LIBRARY.lock().unwrap().as_ref().map(f)
}

pub fn with_library_mut<R>(f: impl FnOnce(&mut LibraryData) -> R) -> Option<R> {
    LIBRARY.lock().unwrap().as_mut().map(f)
}

/// THE heart-state read for a Qobuz entity — the one every publisher and the
/// toggle direction must go through.
///
/// Two sources, in cost order:
///  1. `fav_cache_qt` — O(1), seeded from `favorites_cache.db` at session
///     activation and refreshed by `warm_favorites_cache` at shell entry, so
///     it is populated from first paint and while offline.
///  2. the Library feed — a linear scan, populated only after the user opens
///     the Library view, kept as a fast path in the sense the brief meant it:
///     it is the freshest thing there is once loaded, so a favourite that
///     landed after the warm still lights up.
///
/// The two never disagree for long because every mutation writes BOTH (see
/// `toggle_favorite` / `set_feed_favorite`), which is what makes the OR safe:
/// an un-favourite clears the cache entry AND the feed flag.
///
/// Playlists are not Qobuz favourites at all — their flag lives in
/// `library.db` (see `library_db_qt`) — but they ARE in the cache: it keeps a
/// mirror of that set precisely so this function and
/// `toggle_playlist_favorite` read one authority instead of two. Local / Plex
/// rows are not in the cache (their authority is the local-favorites store)
/// and fall through to the feed.
///
/// COST: the feed arm is a linear scan of up to 10k rows. That is correct for
/// a detail HEADER (one call per page open) and wrong for a card rail — row
/// producers call `fav_cache_qt::is_favorite` directly, which is the O(1) half
/// alone. See its docs.
pub fn is_favorite(kind: &str, id: &str) -> bool {
    if !is_local_feed_id(kind, id) && crate::fav_cache_qt::is_favorite(kind, id) {
        return true;
    }
    // Playlists STOP at the cache. For album/track/artist/label the feed flag
    // and the cache flag answer the same question, so OR-ing the feed in only
    // widens coverage. A playlist row's `is_favorite` used to mean "fetched
    // into the favorites bucket", and even now that it mirrors library.db the
    // feed is a stale snapshot of it — one that a toggle does not refresh. The
    // OR would therefore resurrect a heart the user just cleared, in the one
    // caller this rule sends here (the playlist header), which is precisely
    // the display-vs-direction split the cache exists to close.
    if kind == "playlist" {
        return false;
    }
    with_library(|d| {
        d.feed
            .iter()
            .any(|i| i.kind == kind && i.id == id && i.is_favorite)
    })
    .unwrap_or(false)
}

/// Flip a heart. Qobuz ids route to the favorites API; local/Plex ids to
/// the local-favorites store (library_all.rs `is_local_feed_id` routing);
/// playlists to the local library.db (see `toggle_playlist_favorite`).
///
/// Returns the state the UI should now show: the FLIPPED value on success,
/// the UNCHANGED one when the write failed (so the caller's optimistic flip
/// rolls back instead of leaving a heart lit over a 404), and None only when
/// the kind is unroutable or the store could not be reached at all.
///
/// THE TYPE STRING IS SINGULAR. `/favorite/create` and `/favorite/delete`
/// take `album_ids | artist_ids | track_ids | label_ids | award_ids`
/// (inferred OpenAPI v10.0.0.0-beta, §Favorites), and the client builds the
/// query key as `format!("{fav_type}_ids")` (qbz-qobuz client.rs
/// `add_favorite`/`remove_favorite`) — so the reference passes "track" /
/// "album" / "artist" / "label" everywhere (qbz/src/main.rs:2265, :12082,
/// :12942, :13054). Passing the PLURAL `type` spelling that
/// `/favorite/getUserFavorites` wants produced `artists_ids=…`: an unknown
/// param, so the delete matched nothing and came back 404 Not Found, while
/// the create silently no-opped with the heart left lit. Same word, two
/// endpoints, two conventions — do not unify them.
pub async fn toggle_favorite(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    kind: &str,
    id: &str,
) -> Option<bool> {
    if is_local_feed_id(kind, id) {
        return toggle_local(kind, id);
    }
    if kind == "playlist" {
        return toggle_playlist_favorite(id).await;
    }
    let fav_type = match kind {
        "track" | "album" | "artist" | "label" => kind,
        _ => {
            log::warn!("[qbz-qt] favorite toggle: unroutable kind '{kind}' ({id})");
            return None;
        }
    };
    // Direction from `is_favorite` — cache first, feed second. It used to come
    // from the feed ALONE, which `load_library_once()` fills only when the user
    // opens the Library view (main.rs `navigate_to("library")`): until then
    // every read answered false, so every click sent `favorite/create` — a
    // server no-op — while the UI flipped to filled. Un-favouriting was
    // impossible from every detail page, card and row. The publishers read the
    // same function now, so the drawn heart and the action cannot disagree.
    let current = is_favorite(kind, id);
    let result = if current {
        runtime.core().remove_favorite(fav_type, id).await
    } else {
        runtime.core().add_favorite(fav_type, id).await
    };
    match result {
        Ok(()) => {
            let new_state = !current;
            set_feed_favorite(kind, id, new_state);
            // Mirror to memory + `favorites_cache.db`, so the next page open
            // (and the next launch) agrees with what just happened.
            crate::fav_cache_qt::set(kind, id, new_state);
            Some(new_state)
        }
        Err(e) => {
            log::error!("[qbz-qt] favorite toggle ({kind}:{id}) failed: {e}");
            // Report the UNCHANGED state: the caller flipped optimistically.
            Some(current)
        }
    }
}

/// Every server-side favourite owned by one Qobuz release: its album heart and
/// the hearts on its member tracks. Purchase representatives can carry a heart
/// too, so membership is read from `is_favorite` as well as the favourites
/// group. `(kind,id)` de-duplication prevents a favourite+purchase duplicate
/// from issuing the same delete twice.
fn release_favorite_targets_from(feed: &[FeedItem], album_id: &str) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for item in feed.iter().filter(|item| {
        item.source == "qobuz"
            && (item.is_favorite || item.group == "favorites")
            && ((item.kind == "album" && item.id == album_id)
                || (item.kind == "track" && item.album_id == album_id))
    }) {
        let key = (item.kind.clone(), item.id.clone());
        if seen.insert(key.clone()) {
            targets.push(key);
        }
    }
    targets
}

pub(crate) fn release_favorite_targets(album_id: &str) -> Vec<(String, String)> {
    let mut targets = with_library(|data| release_favorite_targets_from(&data.feed, album_id))
        .unwrap_or_default();
    // A freshly-set album heart can be in the O(1) cache before a Library row
    // has been fetched/reconciled. Include it without taking the LIBRARY lock
    // recursively through `is_favorite`.
    if crate::fav_cache_qt::is_favorite("album", album_id)
        && !targets
            .iter()
            .any(|(kind, id)| kind == "album" && id == album_id)
    {
        targets.push(("album".into(), album_id.into()));
    }
    targets
}

/// The playlist heart is a qbz-LOCAL flag, never a Qobuz favorite — the
/// favorites endpoints have no `playlist_ids` param (inferred OpenAPI
/// §Favorites lists five, none of them playlists), which is exactly why the
/// reference sends this to `db.set_playlist_favorite` instead
/// (qbz/src/main.rs:13652 + `playlist_toggle_favorite_by_id`:2196). Routing
/// it to `/favorite/create` produced `playlists_ids=…`, i.e. a create that
/// changed nothing and a delete that 404'd.
///
/// Direction comes from the db, not from the caller's rendered state: a card
/// cannot know it (main.rs:2198, verbatim reasoning). Follow/unfollow of a
/// FOREIGN playlist is a different action entirely (`playlist/subscribe`,
/// wired in `playlist_qt::toggle_follow`) and is not touched here.
async fn toggle_playlist_favorite(id: &str) -> Option<bool> {
    let Ok(pid) = id.parse::<u64>() else {
        log::warn!("[qbz-qt] playlist favorite: non-Qobuz id '{id}' — refusing");
        return None;
    };
    // rusqlite off the async path (library_db_qt holds a blocking Connection).
    // Read + write in ONE blocking hop so nothing can interleave between the
    // direction read and the write.
    let new_state = tokio::task::spawn_blocking(move || {
        let current = crate::library_db_qt::is_favorite_playlist(pid);
        let next = !current;
        if crate::library_db_qt::set_favorite_playlist(pid, next) {
            // Write-through to the in-memory mirror IN THE SAME HOP, so no
            // card can be built between the db write and the mirror update
            // and read the pre-toggle value.
            crate::fav_cache_qt::set_playlist(pid, next);
            next
        } else {
            log::error!("[qbz-qt] playlist {pid} favorite write failed");
            current
        }
    })
    .await
    .ok()?;
    set_feed_favorite("playlist", id, new_state);
    Some(new_state)
}

/// Local/Plex heart against the local-favorites store. Same contract as
/// `toggle_favorite`: the flipped state on success, the UNCHANGED state when
/// the store write failed (so the caller's optimistic flip rolls back — the
/// `.ok()?` arms used to swallow that as "nothing happened" and the heart
/// kept the flip), None only when there is no store to talk to at all.
fn toggle_local(kind: &str, id: &str) -> Option<bool> {
    let service = LOCAL_FAVS.lock().ok()?;
    let service = service.as_ref()?;
    let current = service.is_favorite(kind, id);
    let new_state = if current {
        if let Err(e) = service.unfavorite(kind, id) {
            log::error!("[qbz-qt] local unfavorite ({kind}:{id}) failed: {e}");
            return Some(current);
        }
        false
    } else {
        // Re-favorite: rebuild the snapshot from the feed row.
        let snap = with_library(|d| {
            d.feed
                .iter()
                .find(|i| i.kind == kind && i.id == id)
                .map(|item| qbz_app::settings::local_favorites::LocalFavItem {
                    kind: kind.to_string(),
                    id: id.to_string(),
                    title: item.title.clone(),
                    subtitle: item.subtitle.clone(),
                    artwork_url: item.image_url.clone(),
                    artist: item.artist.clone(),
                    source: item.source.clone(),
                    favorited_at: 0,
                })
        })
        .flatten();
        let Some(snap) = snap else {
            log::warn!("[qbz-qt] local favorite ({kind}:{id}): no feed row to snapshot");
            return Some(current);
        };
        if let Err(e) = service.favorite(&snap) {
            log::error!("[qbz-qt] local favorite ({kind}:{id}) failed: {e}");
            return Some(current);
        }
        true
    };
    set_feed_favorite(kind, id, new_state);
    Some(new_state)
}

/// Drop every account-scoped Library cache on logout.
///
/// The QML documents are cleared separately, but these are the authorities
/// later actions read. Keeping them alive would let a new session observe the
/// previous account before its first Library reload.
pub fn teardown() {
    if let Ok(mut service) = LOCAL_FAVS.lock() {
        *service = None;
    }
    DELETED_PLAYLISTS.lock().unwrap().clear();
    PLAYLIST_DELETION_REVISION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    *LIBRARY.lock().unwrap() = None;
    LOCAL_RAW.lock().unwrap().clear();
    log::info!("[qbz-qt] library stores cleared (logout)");
}

/// library_all.rs `is_local_feed_id` — local tracks are file paths, local
/// albums group keys, local artists plain names; Qobuz ids are numeric.
///
/// This is the gate `crate::open_album` consults to decide whether an album id
/// goes to the LOCAL detail view or to Qobuz's `/album/get`, and every caller
/// of that router depends on it: the now-playing bar, the queue, "go to album"
/// in a row menu, the Library ALL feed.
pub(crate) fn is_local_feed_id(kind: &str, id: &str) -> bool {
    match kind {
        "track" | "artist" => id.parse::<u64>().is_err(),
        "album" => {
            id.starts_with("logical:")
                || is_server_album_key(id)
                || id.contains('|')
                || id.contains('/')
        }
        _ => false,
    }
}

/// `<source>:<album id>` — the group key every non-Qobuz album carries.
///
/// Spelled through `SourceId::from_word` rather than a list of prefixes: the
/// literal `"plex:"` test this replaces was the whole vocabulary, so a
/// `jellyfin:<itemId>` key answered NO, `open_album` sent it to `/album/get`,
/// and the view landed on an empty album — from the now-playing bar, from the
/// queue, from anywhere that router is reached. Jellyfin item ids are 32-char
/// hex with no `/` and no `|`, so not one of the other clauses caught it.
///
/// `qobuz:` is excluded deliberately: it is a legal word and a catalog id.
fn is_server_album_key(id: &str) -> bool {
    let Some((word, rest)) = id.split_once(':') else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    matches!(
        qbz_source::SourceId::from_word(word),
        Some(s) if s != qbz_source::SourceId::QOBUZ
    )
}

/// main.rs `is_local_album_key` (qbz/src/main.rs:2630-2632) — the PLAY / NAV
/// guard, a SUPERSET of `is_local_feed_id("album", …)`: it also catches the
/// unknown-album bucket. Deliberately a separate helper: `is_local_feed_id` is
/// a byte-for-byte port of the Slint's `library_all.rs:874-880` twin and also
/// decides local-favorites routing, so widening it there would diverge from
/// its reference AND change which API an album literally titled
/// `__unknown_album__` hits.
pub(crate) fn is_local_album_key(id: &str) -> bool {
    is_local_feed_id("album", id) || id == "__unknown_album__"
}

/// Flip the favorite flag on the stored feed row (model-of-truth for the
/// `libraryFavoriteChanged` signal).
///
/// ALL matching rows, not the first: one entity can appear twice in the merged
/// feed under the same `(kind, id)` — a purchased album that is also a
/// favourite lands once in the `favorites` group and once in `purchases`.
/// `find` updated only one of them, and since `is_favorite` answers with
/// `any(...)`, an un-favourite that left the second row lit kept reading
/// "favourite" and the next click re-added it.
pub(crate) fn set_feed_favorite(kind: &str, id: &str, value: bool) {
    with_library_mut(|d| {
        for item in d.feed.iter_mut().filter(|i| i.kind == kind && i.id == id) {
            item.is_favorite = value;
        }
    });
}

// ---------------------------------------------------------------------------
// Artist page "In library" membership (#737 round, contract §3)
// ---------------------------------------------------------------------------
//
// The artist page lists what the user OWNS of one artist: favourites ∪
// purchases, with `Library > Albums` / `Library > Tracks` semantics — NOT the
// `All` feed's. Local / Plex rows never enter (owner ruling 2026-09-04).
//
// One representative row per `{kind}:{id}` — the purchase row wins so its
// quality badge survives — and `is_favorite` on that row answers "is it ALSO
// hearted", which is what the tab's purchases/favourites switches evaluate
// (LibraryView.qml:566-590: neither = union, one = that source, both =
// intersection). `group` on the representative row therefore means
// "is purchased" and `is_favorite` means "is hearted"; a row can carry both.
//
// A track belongs to an artist through its performer OR its album's main
// artist (`FeedItem::album_artist_id`): the feed's `artist_id` is the
// performer, and the owner's own screenshot (Megadeth, two hearted Popular
// Tracks, badge 3) showed hearted tracks dropping out under the
// performer-only rule.

fn feed_row_is_artist(item: &FeedItem, artist_id: &str) -> bool {
    item.artist_id == artist_id || item.album_artist_id == artist_id
}

/// Every album / track of `artist_id` the user owns, deduplicated per entity,
/// in feed order (newest first — `load_library` merges that way).
pub(crate) fn artist_library_items(artist_id: &str) -> Vec<FeedItem> {
    if artist_id.is_empty() {
        return Vec::new();
    }
    with_library(|d| artist_library_items_from(&d.feed, artist_id)).unwrap_or_default()
}

/// The pure rule behind [`artist_library_items`], over any feed slice.
fn artist_library_items_from(feed: &[FeedItem], artist_id: &str) -> Vec<FeedItem> {
    let mut order: Vec<String> = Vec::new();
    let mut by_key: HashMap<String, FeedItem> = HashMap::new();
    for item in feed.iter().filter(|i| {
        (i.kind == "track" || i.kind == "album")
            && (i.group == "favorites" || i.group == "purchases")
            && feed_row_is_artist(i, artist_id)
    }) {
        let key = feed_key(&item.kind, &item.id);
        let hearted = item.group == "favorites" || item.is_favorite;
        match by_key.get_mut(&key) {
            None => {
                let mut row = item.clone();
                row.is_favorite = hearted;
                by_key.insert(key.clone(), row);
                order.push(key);
            }
            Some(row) => {
                row.is_favorite |= hearted;
                // The purchase row is the representative (quality badge,
                // entitlement); keep the heart it just collected.
                if item.group == "purchases" && row.group != "purchases" {
                    let keep_heart = row.is_favorite;
                    *row = item.clone();
                    row.is_favorite = keep_heart;
                }
            }
        }
    }
    order
        .into_iter()
        .filter_map(|k| by_key.remove(&k))
        .collect()
}

/// Reconcile the cached feed with a Qobuz heart that just SETTLED, so a
/// favourite created after `load_library` ran appears in the document and a
/// cleared one leaves it. Returns whether membership changed (the caller then
/// republishes the Library document — the same rule `remove_local_favorite_row`
/// / `insert_local_favorite_row` follow for local hearts).
///
/// Why this exists: `set_feed_favorite` only flips `is_favorite` on rows that
/// are ALREADY in the feed. A track hearted from the artist page had no row,
/// so the artist page's "In library" count never moved until the next
/// session — the reference had the same hole and called it "acceptable v1"
/// (`library_by_artist.rs`); this round closes it.
///
/// - `value == true` and no row: ONE `/track/get` or `/album/get`, mapped by
///   the same producer the full load uses, inserted at the top (newest first).
///   A failed fetch is logged and skipped — the heart itself already landed.
/// - `value == false`: the `favorites` rows leave. A `purchases` row for the
///   same entity STAYS (it is still library) with its heart cleared, which
///   `set_feed_favorite` already did for every row.
///
/// Only Qobuz kinds ride this path; local hearts have their own pair.
pub(crate) async fn reconcile_qobuz_favorite(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    kind: &str,
    id: &str,
    value: bool,
) -> bool {
    if kind != "track" && kind != "album" {
        return false;
    }
    if is_local_feed_id(kind, id) {
        return false;
    }
    // No library snapshot yet: nothing to reconcile, the next load sees the
    // server state.
    if with_library(|_| ()).is_none() {
        return false;
    }
    if !value {
        return with_library_mut(|d| {
            let before = d.feed.len();
            d.feed
                .retain(|i| !(i.group == "favorites" && i.kind == kind && i.id == id));
            let removed = before - d.feed.len();
            if removed > 0 {
                d.counts.all = (d.counts.all - removed as i64).max(0);
                match kind {
                    "track" => d.counts.tracks = (d.counts.tracks - 1).max(0),
                    "album" => d.counts.albums = (d.counts.albums - 1).max(0),
                    _ => {}
                }
            }
            removed > 0
        })
        .unwrap_or(false);
    }
    let already = with_library(|d| {
        d.feed
            .iter()
            .any(|i| i.group == "favorites" && i.kind == kind && i.id == id)
    })
    .unwrap_or(false);
    if already {
        return false;
    }
    let row = match fetch_favorite_row(runtime, kind, id).await {
        Some(row) => row,
        None => {
            log::warn!("[qbz-qt] library feed: could not fetch {kind}:{id} after favourite");
            return false;
        }
    };
    with_library_mut(|d| {
        d.feed.insert(0, row);
        d.counts.all += 1;
        match kind {
            "track" => d.counts.tracks += 1,
            "album" => d.counts.albums += 1,
            _ => {}
        }
    });
    true
}

/// The catalog row for a freshly hearted entity, mapped by the SAME producer
/// the full load uses (`map_track` / `map_album`), so a row inserted live and
/// one fetched at load are indistinguishable downstream.
async fn fetch_favorite_row(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    kind: &str,
    id: &str,
) -> Option<FeedItem> {
    match kind {
        "track" => {
            let tid = id.parse::<u64>().ok()?;
            let track = runtime.core().get_track(tid).await.ok()?;
            let mut row = map_track(track);
            row.is_favorite = true;
            Some(row)
        }
        "album" => {
            let album = runtime.core().get_album(id).await.ok()?;
            let ready = ready_offline_album_track_counts().await;
            let mut row = map_album(album, ready.get(id).copied().unwrap_or(0));
            row.is_favorite = true;
            Some(row)
        }
        _ => None,
    }
}

/// Reconcile a local-favorite membership change with the cached mixed feed.
/// In the `all` local scope the live row stays and only its heart changes; in
/// the `favorites` scope the row itself joins/leaves the document.
pub(crate) fn remove_local_favorite_row(kind: &str, id: &str) -> bool {
    if crate::library_prefs::local_scope() != "favorites" {
        return false;
    }
    with_library_mut(|data| remove_local_favorite_row_from(data, kind, id)).unwrap_or(false)
}

fn remove_local_favorite_row_from(data: &mut LibraryData, kind: &str, id: &str) -> bool {
    let before = data.feed.len();
    data.feed
        .retain(|item| !(item.group == "local" && item.kind == kind && item.id == id));
    let removed = before - data.feed.len();
    if removed == 0 {
        return false;
    }
    data.counts.all = (data.counts.all - removed as i64).max(0);
    true
}

/// Live counterpart of [`remove_local_favorite_row`], used when the heart was
/// clicked on a Local Library card rather than on a row already in this feed.
pub(crate) fn insert_local_favorite_row(item: &LocalFavItem) -> bool {
    if crate::library_prefs::local_scope() != "favorites" {
        set_feed_favorite(&item.kind, &item.id, true);
        return false;
    }
    with_library_mut(|data| insert_local_favorite_row_into(data, item)).unwrap_or(false)
}

fn insert_local_favorite_row_into(data: &mut LibraryData, item: &LocalFavItem) -> bool {
    let mut already_present = false;
    for row in data
        .feed
        .iter_mut()
        .filter(|row| row.kind == item.kind && row.id == item.id)
    {
        row.is_favorite = true;
        already_present |= row.group == "local";
    }
    if already_present {
        return false;
    }
    data.feed.insert(
        0,
        FeedItem {
            kind: item.kind.clone(),
            group: "local".into(),
            source: item.source.clone(),
            id: item.id.clone(),
            title: item.title.clone(),
            subtitle: item.subtitle.clone(),
            artist: item.artist.clone(),
            image_url: item.artwork_url.clone(),
            art_key: feed_key(&item.kind, &item.id),
            is_favorite: true,
            is_pinned: crate::sidebar_qt::is_pinned(&item.kind, &item.id),
            added_rank: 0.0,
            ..Default::default()
        },
    );
    data.counts.all += 1;
    true
}

/// An UNFOLLOW just landed: drop the playlist's rows from the live Library
/// document. Returns whether anything was removed (so the caller only
/// republishes when it must).
///
/// Why the feed is mutated instead of left to the next reload: unfollowing is
/// the user asking for the playlist to leave their library, and `load_library`
/// only re-runs when the view is (re)entered — so the row sat there, still
/// listed, until a manual refresh. That is the owner's report.
///
/// ALL matching rows, not the first: a foreign playlist that is ALSO hearted
/// locally is pushed TWICE by `load_library` (once from the not-owned pass,
/// :553-560, once from the hearted-ids pass, :576-585 — both with
/// `is_following = true`, so `group` cannot tell them apart), and leaving one
/// of them behind would leave the row on screen.
///
/// HEARTED IS NOT UNFOLLOWED. The local heart is a qbz-only flag in library.db
/// and it survives the unsubscribe, so the state the NEXT full load produces
/// for a hearted-and-unfollowed playlist is exactly ONE row: `get_user_playlists`
/// no longer lists it, the hearted-ids pass falls through to `get_playlist(fid)`
/// and maps it with `is_following = false` — a `favorites` row (`load_library`
/// :580-584). The reference surfaces the same "favorited but neither owned nor
/// subscribed" playlist deliberately (`qbz/src/playlist_manager.rs:212-215`).
/// So this collapses to that one row instead of removing it: dropping it would
/// make the playlist vanish now and reappear at the next load, which is the
/// owner's original complaint wearing the other shoe.
///
/// `counts.all` follows the rows that actually left. `counts.playlists` is the
/// FAVORITES bucket (`load_library`: `playlists_total = pl_favorites.len()` =
/// owned ∪ locally hearted) — an unfollow only ever targets a FOREIGN playlist,
/// so it was in that bucket iff it is hearted, and in that case the row STAYS.
/// Either way the count is unchanged. Clamped: the badge must never go negative.
pub(crate) fn remove_playlist_rows(id: &str) -> bool {
    let hearted = crate::fav_cache_qt::is_favorite("playlist", id);
    with_library_mut(|d| {
        // Snapshot one row before the retain — it is the only place the
        // playlist's title / covers / subtitle still exist without a refetch.
        let kept = if hearted {
            d.feed
                .iter()
                .find(|i| i.kind == "playlist" && i.id == id)
                .cloned()
        } else {
            None
        };
        let before = d.feed.len();
        d.feed.retain(|i| !(i.kind == "playlist" && i.id == id));
        let removed = before - d.feed.len();
        if removed == 0 {
            return false;
        }
        d.counts.all = (d.counts.all - removed as i64).max(0);
        if let Some(mut row) = kept {
            // Re-filed exactly as the next `load_library` will map it.
            row.group = "favorites".into();
            row.playlist_following = false;
            row.playlist_owned = false;
            d.feed.insert(0, row);
            d.counts.all += 1;
        }
        true
    })
    .unwrap_or(false)
}

/// A playlist just JOINED the user's library (a follow, or a copy that created
/// an owned one): put it into the live Library document so it appears without a
/// manual refresh. Returns whether the document changed.
///
/// The mirror of [`remove_playlist_rows`], and it takes the pieces rather than
/// a `&Playlist` because its callers hold a mapped document, not the raw API
/// model — re-fetching the playlist just to re-map it would pull its whole
/// track list back over the wire for one card. The fields it does fill are
/// produced by the SAME helpers `map_playlist_row` uses ([`playlist_subtitle`],
/// `fav_cache_qt`, `sidebar_qt::is_pinned`), so the synthesized row and the one
/// the next full load produces agree.
///
/// `following` picks the bucket exactly as `map_playlist_row` does: a followed
/// playlist is a `following` row, an owned one a `favorites` row — which is the
/// sub-tab the Library's Playlists tab opens on.
///
/// `counts.playlists` follows only the owned arm, because that is the count
/// `load_library` publishes (`pl_favorites.len()`).
///
/// No-op when the Library was never loaded this session (`LIBRARY` is `None`) —
/// there is no document to patch and the eventual load fetches the truth.
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_playlist_row(
    id: &str,
    title: &str,
    owner: &str,
    tracks_count: u32,
    cover_url: &str,
    covers: Vec<String>,
    following: bool,
) -> bool {
    with_library_mut(|d| {
        if d.feed.iter().any(|i| i.kind == "playlist" && i.id == id) {
            return false;
        }
        let item = FeedItem {
            is_pinned: crate::sidebar_qt::is_pinned("playlist", id),
            kind: "playlist".into(),
            group: if following { "following" } else { "favorites" }.into(),
            source: "qobuz".into(),
            id: id.to_string(),
            title: title.to_string(),
            subtitle: playlist_subtitle(owner, tracks_count),
            track_count: Some(tracks_count),
            playlist_own_image: !cover_url.is_empty(),
            image_url: cover_url.to_string(),
            covers,
            is_favorite: crate::fav_cache_qt::is_favorite("playlist", id),
            playlist_owned: !following,
            playlist_creator: playlist_creator(!following, owner).into(),
            playlist_following: following,
            // 0.0 = most-recently added, which is what this just made it. The
            // feed is kept sorted by this proxy, so the row lands at the head
            // of the All tab exactly like the newest favourite does.
            added_rank: 0.0,
            ..Default::default()
        }
        .keyed();
        d.feed.insert(0, item);
        d.counts.all += 1;
        if !following {
            d.counts.playlists += 1;
        }
        true
    })
    .unwrap_or(false)
}

/// A pin/unpin just landed: patch the cached feed rows so a later
/// serialization of this document does not resurrect the stale flag.
///
/// Twin of `home_qt::apply_pin_change` / `search_qt::apply_pin_change`, and
/// like them it publishes NOTHING: the rows on screen are corrected in place
/// by `QbzLibrary.pinChanged`, which `LibraryView.qml` listens to, and
/// re-serializing a 10k-row feed per pin click is exactly the delegate-model
/// teardown this split exists to avoid.
pub(crate) fn apply_pin_change(kind: &str, id: &str, pinned: bool) {
    with_library_mut(|d| {
        for item in d.feed.iter_mut().filter(|i| i.kind == kind && i.id == id) {
            item.is_pinned = pinned;
        }
    });
}

// ---------------------------------------------------------------------------
// Per-row play: the VISIBLE list becomes the queue (PARITY-DEBT #5)
// ---------------------------------------------------------------------------
//
// Reference: `playback.rs::play_track_in_context` (:3459) — the ONE entry point
// every Slint tracklist row goes through. Its Favorites arm is
//
//     ContentView::Favorites => {
//         if let Some((tracks, idx)) = order_by_visible(
//             &FavoritesState.tracks_visible,     // what the list RENDERS
//             crate::favorites::play_tracks(),    // FAV_CURRENT, the authority
//             clicked_id,
//         ) { play_tracks(runtime, weak, handle, tracks, idx); return; }
//     }
//     // …no resolvable list context: play just the track.
//     if let Ok(tid) = clicked_id.parse::<u64>() { play_track_now(…) }
//
// and `order_by_visible` (playback.rs:3408-3428) is: take the ids in VISIBLE
// order, resolve each against the authoritative cache (unresolvable rows are
// dropped), then locate the clicked id INSIDE that ordered list — `None` when
// it is not there, so the caller plays the single track instead of starting
// the queue at the wrong row.
//
// Port shape: the port has ONE merged feed (`LibraryData.feed`) and derives the
// rendered list in QML, so the visible ORDER can only come from QML — it
// carries the tab, the search, the sort and the genre/source filters the user
// is actually looking at, which is the whole point of ordering by the visible
// model rather than by the cache. QML passes that order down as a JSON id
// array; the feed plays the part of `FAV_CURRENT`.
//
// One deliberate widening, called out because it is a deviation: the Slint
// always orders by the TRACKS-tab model, so a click in its mixed All feed
// queues the Tracks tab (and plays a lone track when that tab was never
// opened and its model is still empty). Here the visible list IS the list
// under the pointer — identical on the Tracks tab, and in the All feed it
// queues the mixed feed's track rows in the order shown instead of nothing.

/// Every source word whose row id is NOT a Qobuz catalog id.
///
/// The list is an ALLOWLIST rather than `!= "qobuz"` because the fall-through
/// below parses the id as a u64 and resolves it against the Qobuz catalog: a
/// word this function has not been taught silently becomes "somebody else's
/// track", which is exactly the failure the test below pins for local/plex.
/// Media-server tracks do not ride this feed today (only their ALBUMS do), so
/// this is the guard being taught the words BEFORE they arrive, not a fix for
/// a live bug.
fn is_local_source(source: &str) -> bool {
    matches!(
        source,
        "local" | "plex" | "jellyfin" | "subsonic" | "navidrome" | "gonic" | "airsonic" | "astiga"
    )
}

/// `playback_qt::feed_queue_track` for a feed row already in hand.
///
/// Mirrors that function field-for-field (it looks the same row up by id and
/// is private to `playback_qt`); the follow-up is to make ONE of them
/// `pub(crate)` and delete the other.
///
/// A `local` / `plex` row is NOT resolved here — it goes to
/// `local_playback::local_queue_track` through the [`LOCAL_RAW`] cache, which
/// is the only thing that knows its file path, its Plex rating key and its
/// real source discriminator.
///
/// That branch is load-bearing, not defensive. The `"all"` local scope emits
/// `local_tracks.id` — a small integer — as the row id, and this function's
/// first line parses the id as a u64: without the branch, clicking a local
/// track would have queued the QOBUZ track that happens to carry that number.
/// The old doc here assumed every local row's id was a path and could only be
/// dropped; that was true of the hearted-only feed, and stopped being true the
/// moment the whole library could enter it.
///
/// `None` when the row cannot be resolved (a hearted local row while the scope
/// is "favorites" has no raw row cached). Dropping it is the same queue the
/// Slint builds — its `FAV_CURRENT` is Qobuz-only — and it is strictly better
/// than resolving it as somebody else's track.
pub(crate) fn feed_track_to_queue(item: &FeedItem) -> Option<QueueTrack> {
    if is_local_source(&item.source) {
        let row_id = item.id.parse::<i64>().ok()?;
        let raw = LOCAL_RAW.lock().ok()?.get(&row_id).cloned();
        return match raw {
            Some(t) => Some(crate::local_playback::local_queue_track(&t)),
            None => {
                log::debug!(
                    "[qbz-qt] library feed: local row {} ({}) has no raw track cached",
                    item.id,
                    item.title
                );
                None
            }
        };
    }
    let id = item.id.parse::<u64>().ok()?;
    Some(QueueTrack {
        id,
        title: item.title.clone(),
        version: None,
        artist: item.artist.clone(),
        album: item.album.clone(),
        album_version: None,
        duration_secs: duration_secs(&item.duration),
        artwork_url: if item.image_url.is_empty() {
            None
        } else {
            Some(item.image_url.clone())
        },
        // playback.rs `make_queue_track` (:2426): the CATALOG max travels with
        // the queue track. `None` here zeroed `quality_state`'s `TRACK_MAX_*`
        // seed, so a Library track play drew a bare tier on the NPB AudioStamp
        // with no "24-bit / 96 kHz" line.
        hires: item.quality_tier == "hires",
        bit_depth: item.bit_depth,
        sample_rate: item.sample_rate,
        is_local: false,
        album_id: if item.album_id.is_empty() {
            None
        } else {
            Some(item.album_id.clone())
        },
        artist_id: item.artist_id.parse::<u64>().ok(),
        // D5: the feed row's own answer. Twin of `playback_qt::feed_queue_track`
        // over the SAME `FeedItem` — both read this field rather than hardcoding
        // a yes, so a Library row and a queue row cannot disagree.
        streamable: !item.not_streamable,
        source: Some("qobuz".to_string()),
        parental_warning: item.explicit,
        source_item_id_hint: None,
        context_kind: None,
        context_id: None,
        isrc: None,
        recording_mbid: None,
    })
}

/// Inverse of `mmss` — the feed carries the duration as the rendered `M:SS`
/// string, so the queue row has to read it back. Anything unparseable is 0,
/// which is what `playback_qt::feed_queue_track` does too (the engine
/// re-reports the real duration once the track resolves).
fn duration_secs(mmss: &str) -> u64 {
    let mut parts = mmss.split(':');
    parts
        .next()
        .and_then(|m| m.parse::<u64>().ok())
        .unwrap_or(0)
        * 60
        + parts
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
}

/// `playback.rs::order_by_visible` (:3408-3428), ported: build the queue in
/// VISIBLE order out of the authoritative feed and anchor it on `clicked_id`.
///
/// `None` — and the caller then plays the single track — when the clicked id
/// does not resolve inside the ordered list: an orphan row, a cache miss, or a
/// local row that carries no numeric id. Same guard, same reason: better one
/// track than a queue that starts on the wrong one.
fn order_by_visible(
    feed: &[FeedItem],
    visible_ids: &[String],
    clicked_id: &str,
) -> Option<(Vec<QueueTrack>, usize)> {
    let by_id: std::collections::HashMap<&str, &FeedItem> = feed
        .iter()
        .filter(|i| i.kind == "track")
        .map(|i| (i.id.as_str(), i))
        .collect();
    let ordered: Vec<QueueTrack> = visible_ids
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).copied())
        .filter_map(feed_track_to_queue)
        .collect();
    let idx = ordered
        .iter()
        .position(|t| t.id.to_string() == clicked_id)?;
    Some((ordered, idx))
}

/// LibraryView row play. `visible_ids_json` is the JSON string array of the
/// track ids the view is CURRENTLY rendering, in render order (the
/// `libraryArtworkWindow` convention); `clicked_id` is the row that was hit.
///
/// Fire-and-forget like every other `*_qt` play entry point (`label_qt::
/// play_track`): the queue is built synchronously off the feed, the play is
/// spawned. Blacklisted rows AND rows Qobuz pulled (contract §5.3 D5) are
/// dropped inside `playback_qt::set_queue_stamped`, which also remaps the start
/// index and hands back the id it actually anchored on — so nothing to do here
/// beyond passing the list through `play_track_list`.
/// Library header "Play all tracks" / "Shuffle all tracks" — the visible set
/// as the queue, from the top (`FavoritesActions.play_all_tracks` /
/// `shuffle_tracks`).
///
/// `shuffle` REORDERS this queue; it does not switch the player's shuffle mode
/// (PARITY-DEBT #17 — the reference's `favorites::shuffled_tracks()` mixes the
/// Vec and plays index 0, and latching the global toggle here would change
/// everything played afterwards).
pub fn play_visible_all(visible_ids_json: String, shuffle: bool) {
    let visible_ids: Vec<String> = serde_json::from_str(&visible_ids_json).unwrap_or_default();
    let Some(first) = visible_ids.first().cloned() else {
        return;
    };
    let queued = with_library(|d| order_by_visible(&d.feed, &visible_ids, &first)).flatten();
    let Some((queue, _)) = queued else {
        log::info!("[qbz-qt] library play-all: the visible list resolved to nothing");
        return;
    };
    log::info!(
        "[qbz-qt] library play-all: queueing {} visible track(s) (shuffle={shuffle})",
        queue.len()
    );
    let runtime = crate::app();
    crate::spawn(async move {
        if let Err(e) = crate::playback_qt::play_track_list(&runtime, queue, 0, shuffle).await {
            log::error!("[qbz-qt] library play-all failed: {e}");
        }
    });
}

pub fn play_from_visible(visible_ids_json: String, clicked_id: String) {
    let visible_ids: Vec<String> = serde_json::from_str(&visible_ids_json).unwrap_or_default();
    let queued = with_library(|d| order_by_visible(&d.feed, &visible_ids, &clicked_id)).flatten();
    let Some((queue, start)) = queued else {
        // order_by_visible == None: the Slint's fallback is the single track
        // (playback.rs:3620-3623).
        log::info!(
            "[qbz-qt] library play: {clicked_id} does not resolve in the visible list \
             ({} rows) -> single track",
            visible_ids.len()
        );
        if let Ok(id) = clicked_id.parse::<u64>() {
            crate::play_track(id);
        }
        return;
    };
    log::info!(
        "[qbz-qt] library play: queueing {} visible track(s) from index {start}",
        queue.len()
    );
    let runtime = crate::app();
    crate::spawn(async move {
        if let Err(e) = crate::playback_qt::play_track_list(&runtime, queue, start, false).await {
            log::error!("[qbz-qt] library play_from_visible failed: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purchased_album_sort_metadata_is_preserved() {
        let album = serde_json::from_value(serde_json::json!({
            "id": "a", "title": "A", "release_date_original": "1980-04-01",
            "duration": 2500, "tracks_count": 11, "label": {"id": 1, "name": "Label"}
        })).unwrap();
        let row = map_purchased_album(album);
        assert_eq!(row.release_sort_key, 19800401);
        assert_eq!(row.duration_secs, 2500);
        assert_eq!(row.track_count, Some(11));
        assert_eq!(row.label, "Label");
    }

    #[test]
    fn track_sort_metadata_uses_original_date_and_keeps_missing_dates_unknown() {
        let track: Track = serde_json::from_value(serde_json::json!({
            "id": 42, "duration": 234,
            "album": {"id": "a", "title": "A", "release_date_original": "1980-04-01",
                "release_date_stream": "2015-06-01", "label": {"id": 1, "name": "Label"}}
        })).unwrap();
        let row = map_track(track);
        assert_eq!(row.release_sort_key, 19800401);
        assert_eq!(row.label, "Label");
        assert_eq!(row.duration_secs, 234);
        let track: Track = serde_json::from_value(serde_json::json!({
            "id": 43, "album": {"release_date_stream": "2015-06-01"}
        })).unwrap();
        assert_eq!(map_track(track).release_sort_key, 0);
    }

    #[test]
    fn playlist_sort_metadata_retains_numeric_values_and_empty_track_count() {
        let playlist: Playlist = serde_json::from_value(serde_json::json!({
            "id": 42, "name": "List", "tracks_count": 0, "duration": 0,
            "updated_at": 1700000000
        })).unwrap();
        let row = serde_json::to_value(map_playlist_row(&playlist, false)).unwrap();
        assert_eq!(row["trackCount"], 0);
        assert_eq!(row["updatedAt"], 1700000000);
    }

    #[test]
    fn playlist_creator_filters_distinguish_ownership_and_editorial_names() {
        assert_eq!(playlist_creator(true, "Qobuz fan"), "you");
        assert_eq!(playlist_creator(false, "Qobuz"), "qobuz");
        assert_eq!(playlist_creator(false, "Qobuz Latinoamérica"), "qobuz");
        assert_eq!(playlist_creator(false, "My Qobuz playlist"), "others");
        assert_eq!(playlist_creator(false, "Listener"), "others");
        assert_eq!(playlist_creator(false, ""), "unknown");
    }

    #[tokio::test]
    async fn favorites_paging_passes_ten_thousand_and_preserves_every_track() {
        for reported_total in [0, 10_751] {
            let mut offsets = Vec::new();
            let (items, total) = collect_favorite_pages("tracks", |offset| {
                offsets.push(offset);
                let end = (offset + PAGE_SIZE).min(10_751);
                let items: Vec<_> = (offset..end)
                    .map(|id| serde_json::json!({"id": id}))
                    .collect();
                std::future::ready(Ok(serde_json::json!({
                    "tracks": {"items": items, "total": reported_total}
                })))
            })
            .await
            .unwrap();
            assert_eq!(items.len(), 10_751);
            assert_eq!(total, reported_total);
            assert_eq!(offsets.last(), Some(&10_500));
            assert!(items.iter().enumerate().all(|(id, item)| item["id"] == id));
        }
    }

    #[tokio::test]
    async fn favorites_paging_stops_at_total_or_empty_page_and_propagates_errors() {
        let mut calls = 0;
        let (items, _) = collect_favorite_pages("tracks", |offset| {
            calls += 1;
            assert_eq!(offset, 0);
            std::future::ready(Ok(serde_json::json!({"tracks": {
                "total": PAGE_SIZE,
                "items": (0..PAGE_SIZE).map(|id| serde_json::json!({"id":id})).collect::<Vec<_>>()
            }})))
        })
        .await
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(items.len(), PAGE_SIZE as usize);
        let (items, _) = collect_favorite_pages("tracks", |_| {
            std::future::ready(Ok(serde_json::json!({"tracks":{"items":[],"total":0}})))
        })
        .await
        .unwrap();
        assert!(items.is_empty());
        let error = collect_favorite_pages("tracks", |_| {
            std::future::ready(Err("API failed".to_string()))
        })
        .await
        .unwrap_err();
        assert_eq!(error, "API failed");
    }

    #[tokio::test]
    async fn favorites_paging_rejects_a_server_ignoring_offset() {
        let mut calls = 0;
        let error = collect_favorite_pages("tracks", |_| {
            calls += 1;
            std::future::ready(Ok(serde_json::json!({"tracks": {
                "total": 0,
                "items": (0..PAGE_SIZE).map(|id| serde_json::json!({"id":id})).collect::<Vec<_>>()
            }})))
        })
        .await
        .unwrap_err();
        assert_eq!(calls, 2);
        assert!(error.contains("repeated a page at offset 500"));
    }

    #[test]
    fn album_sort_key_prefers_original_over_digital_reissue_dates() {
        let album: Album = serde_json::from_value(serde_json::json!({
            "id":"souls", "title":"The Book of Souls",
            "release_date_original":"2015-09-04",
            "dates":{"stream":"2026-09-08", "download":"2025-01-01"}
        }))
        .unwrap();
        let item = map_album(album, 0);
        assert_eq!(item.release_sort_key, 20150904);
        assert!(item.year.ends_with("2015"));
        assert_eq!(
            serde_json::to_value(item).unwrap()["releaseSortKey"],
            20150904
        );
    }

    #[test]
    fn purchased_albums_carry_visible_dates_and_chronological_keys() {
        for (date, expected) in [
            (Some("2015-09-04"), 20150904),
            (Some("2021-09-03"), 20210903),
            (None, 0),
        ] {
            let album = serde_json::from_value(serde_json::json!({
                "id":"purchased", "title":"Purchased album", "release_date_original":date
            }))
            .unwrap();
            let row = map_purchased_album(album);
            assert_eq!(row.release_sort_key, expected);
            assert_eq!(row.year, qbz_text_utils::dates::release_label(date));
            if expected != 0 {
                assert!(!row.year.is_empty());
                assert_eq!(
                    serde_json::to_value(row).unwrap()["releaseSortKey"],
                    expected
                );
            }
        }
    }

    fn track(id: &str, title: &str) -> FeedItem {
        FeedItem {
            kind: "track".into(),
            id: id.into(),
            title: title.into(),
            duration: "3:07".into(),
            ..Default::default()
        }
    }

    #[test]
    fn embedded_release_classification_is_three_way_and_future_safe() {
        let unknown: Track =
            serde_json::from_str(r#"{"id":1,"streamable":false,"album":{"id":"a","title":"A"}}"#)
                .unwrap();
        assert_eq!(
            embedded_release_availability(&unknown),
            ReleaseAvailability::Unknown
        );

        let live: Track = serde_json::from_str(
            r#"{"id":1,"streamable":false,"album":{"id":"a","title":"A","streamable":true}}"#,
        )
        .unwrap();
        assert_eq!(
            embedded_release_availability(&live),
            ReleaseAvailability::Available
        );

        let gone: Track = serde_json::from_str(
            r#"{"id":1,"streamable":false,"album":{"id":"a","title":"A","streamable":false}}"#,
        )
        .unwrap();
        assert_eq!(
            embedded_release_availability(&gone),
            ReleaseAvailability::Unavailable
        );

        let future: Track = serde_json::from_str(
            r#"{"id":1,"streamable":false,"album":{"id":"a","title":"A","streamable":false,"release_date_stream":"2999-01-01"}}"#,
        )
        .unwrap();
        assert_eq!(
            embedded_release_availability(&future),
            ReleaseAvailability::Available
        );
    }

    #[test]
    fn fetched_release_needs_positive_withdrawal_evidence() {
        let missing_signal: Album = serde_json::from_str(r#"{"id":"a","title":"A"}"#).unwrap();
        assert!(!fetched_release_unavailable(&missing_signal));

        let album_false: Album =
            serde_json::from_str(r#"{"id":"a","title":"A","streamable":false}"#).unwrap();
        assert!(fetched_release_unavailable(&album_false));

        let all_tracks_false: Album = serde_json::from_str(
            r#"{"id":"a","title":"A","streamable":true,"tracks":{"items":[{"id":1,"streamable":false},{"id":2,"streamable":false}],"total":2}}"#,
        )
        .unwrap();
        assert!(fetched_release_unavailable(&all_tracks_false));

        let one_live: Album = serde_json::from_str(
            r#"{"id":"a","title":"A","streamable":true,"tracks":{"items":[{"id":1,"streamable":false},{"id":2,"streamable":true}],"total":2}}"#,
        )
        .unwrap();
        assert!(!fetched_release_unavailable(&one_live));
    }

    #[test]
    fn release_cleanup_targets_album_and_tracks_once_but_not_purchases_alone() {
        let mut album_favorite = album("old-release");
        album_favorite.source = "qobuz".into();
        album_favorite.group = "favorites".into();
        album_favorite.is_favorite = true;

        let mut album_purchase_duplicate = album_favorite.clone();
        album_purchase_duplicate.group = "purchases".into();

        let mut favorite_track = track("11", "One");
        favorite_track.source = "qobuz".into();
        favorite_track.group = "favorites".into();
        favorite_track.album_id = "old-release".into();
        favorite_track.is_favorite = true;

        let mut purchase_only = track("12", "Two");
        purchase_only.source = "qobuz".into();
        purchase_only.group = "purchases".into();
        purchase_only.album_id = "old-release".into();
        purchase_only.is_favorite = false;

        let targets = release_favorite_targets_from(
            &[
                album_favorite,
                album_purchase_duplicate,
                favorite_track,
                purchase_only,
            ],
            "old-release",
        );
        assert_eq!(
            targets,
            vec![
                ("album".to_string(), "old-release".to_string()),
                ("track".to_string(), "11".to_string()),
            ]
        );
    }

    /// A server album key must route to the LOCAL detail view.
    ///
    /// The regression this pins: `open_album` asks `is_local_feed_id`, which
    /// tested the literal prefix `"plex:"`. A `jellyfin:<itemId>` key answered
    /// NO and went to Qobuz's `/album/get`, which 404s — the view navigated
    /// and landed EMPTY. Owner report, 2026-08-20.
    #[test]
    fn a_server_album_key_is_a_local_id() {
        for id in [
            "plex:abc123",
            "jellyfin:1f4e9a0b2c3d4e5f6a7b8c9d0e1f2a3b",
            "subsonic:al-42",
            "navidrome:0f9c",
            "gonic:77",
            "airsonic:al-1",
            "astiga:9",
        ] {
            assert!(is_local_feed_id("album", id), "{id} must route locally");
        }
        // The controls: a Qobuz catalog id, and a word that is NOT a source.
        assert!(!is_local_feed_id("album", "0060254727511"));
        assert!(!is_local_feed_id("album", "qobuz:0060254727511"));
        assert!(!is_local_feed_id("album", "spotify:abc"));
        // A bare word with no id behind it is not a key.
        assert!(!is_local_feed_id("album", "jellyfin:"));
        // Still true for the two shapes that always worked.
        assert!(is_local_feed_id("album", "Artist|Album"));
        assert!(is_local_feed_id("album", "/music/Artist/Album"));
        assert!(is_local_feed_id("album", "logical:0123456789abcdef"));
    }

    /// The branch that keeps a local row out of the Qobuz resolver. A local
    /// track id is a `local_tracks.id` — a small integer that is ALSO a valid
    /// Qobuz track id — so the guard cannot be "does it parse as a number":
    /// it has to be the source. With no raw row cached the row is dropped,
    /// which is the queue the Slint builds; what must never happen is a
    /// `QueueTrack` with `source: "qobuz"` and this id.
    #[test]
    fn a_local_row_is_never_resolved_as_a_qobuz_track() {
        for source in [
            "local",
            "plex",
            "jellyfin",
            "subsonic",
            "navidrome",
            "gonic",
            "airsonic",
            "astiga",
        ] {
            let mut item = track("42", "Ghost in the Machine");
            item.source = source.into();
            assert!(
                feed_track_to_queue(&item).is_none(),
                "{source} row with no cached raw track must drop, not resolve"
            );
        }
        // The control: the same numeric id from a Qobuz row still resolves.
        let mut qobuz = track("42", "Ghost in the Machine");
        qobuz.source = "qobuz".into();
        assert_eq!(feed_track_to_queue(&qobuz).expect("qobuz row").id, 42);
    }

    fn album(id: &str) -> FeedItem {
        FeedItem {
            kind: "album".into(),
            id: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn duration_secs_reads_back_mmss() {
        // `mmss` never emits hours — 75 minutes renders as "75:04".
        assert_eq!(duration_secs("3:07"), 187);
        assert_eq!(duration_secs("0:59"), 59);
        assert_eq!(duration_secs("75:04"), 4504);
        assert_eq!(duration_secs(""), 0);
        assert_eq!(duration_secs("x:y"), 0);
    }

    #[test]
    fn queue_follows_the_visible_order_not_the_feed_order() {
        let feed = vec![track("1", "a"), track("2", "b"), track("3", "c")];
        // What the user is looking at: sorted by title descending.
        let visible = vec!["3".to_string(), "2".to_string(), "1".to_string()];
        let (queue, start) = order_by_visible(&feed, &visible, "2").expect("clicked row resolves");
        assert_eq!(
            queue.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![3, 2, 1]
        );
        assert_eq!(start, 1);
    }

    #[test]
    fn non_track_and_unresolvable_rows_drop_out() {
        let feed = vec![track("1", "a"), album("2"), track("9", "z")];
        // The All feed's visible order: a track, an album row, a local track
        // (path id), a track that is not in the feed at all.
        let visible = vec![
            "1".to_string(),
            "2".to_string(),
            "/home/u/x.flac".to_string(),
            "404".to_string(),
            "9".to_string(),
        ];
        let (queue, start) = order_by_visible(&feed, &visible, "9").expect("clicked row resolves");
        assert_eq!(queue.iter().map(|t| t.id).collect::<Vec<_>>(), vec![1, 9]);
        assert_eq!(start, 1);
    }

    #[test]
    fn clicked_row_outside_the_visible_list_is_none() {
        let feed = vec![track("1", "a"), track("2", "b")];
        let visible = vec!["1".to_string()];
        // Reference: the caller then plays the single track rather than
        // starting the queue at the wrong row.
        assert!(order_by_visible(&feed, &visible, "2").is_none());
        // A local row is unresolvable even when it IS visible.
        assert!(
            order_by_visible(&feed, &["/home/u/x.flac".to_string()], "/home/u/x.flac").is_none()
        );
        // Nothing visible at all.
        assert!(order_by_visible(&feed, &[], "1").is_none());
    }

    #[test]
    fn queue_row_carries_the_feed_metadata() {
        let mut item = track("42", "Title");
        item.artist = "Artist".into();
        item.artist_id = "7".into();
        item.album = "Album".into();
        item.album_id = "abc".into();
        item.image_url = "https://x/1.jpg".into();
        item.quality_tier = "hires".into();
        item.explicit = true;
        let qt = feed_track_to_queue(&item).expect("numeric id");
        assert_eq!(qt.id, 42);
        assert_eq!(qt.title, "Title");
        assert_eq!(qt.artist_id, Some(7));
        assert_eq!(qt.album_id.as_deref(), Some("abc"));
        assert_eq!(qt.artwork_url.as_deref(), Some("https://x/1.jpg"));
        assert_eq!(qt.duration_secs, 187);
        assert!(qt.hires);
        assert!(qt.parental_warning);
        assert_eq!(qt.source.as_deref(), Some("qobuz"));
        // Empty strings become None, never Some("") — the now-playing card
        // tests `is_some()`.
        let bare = track("43", "T");
        let qt = feed_track_to_queue(&bare).expect("numeric id");
        assert!(qt.artwork_url.is_none());
        assert!(qt.album_id.is_none());
        assert!(qt.artist_id.is_none());
        assert!(!qt.hires);
    }

    fn empty_library(feed: Vec<FeedItem>) -> LibraryData {
        LibraryData {
            counts: LibraryCounts {
                tracks: 0,
                albums: 0,
                artists: 0,
                playlists: 0,
                labels: 0,
                all: feed.len() as i64,
            },
            feed,
        }
    }

    #[test]
    fn local_favorite_membership_adds_and_removes_the_row() {
        let snapshot = LocalFavItem {
            kind: "album".into(),
            id: "/music/Artist/Album".into(),
            title: "Album".into(),
            subtitle: "Artist".into(),
            artwork_url: "file:///cover.jpg".into(),
            artist: "Artist".into(),
            source: "local".into(),
            favorited_at: 0,
        };
        let mut data = empty_library(Vec::new());
        assert!(insert_local_favorite_row_into(&mut data, &snapshot));
        assert_eq!(data.counts.all, 1);
        assert_eq!(data.feed[0].art_key, "album:/music/Artist/Album");
        assert!(data.feed[0].is_favorite);
        assert!(!insert_local_favorite_row_into(&mut data, &snapshot));
        assert!(remove_local_favorite_row_from(
            &mut data,
            "album",
            "/music/Artist/Album"
        ));
        assert_eq!(data.counts.all, 0);
        assert!(data.feed.is_empty());
    }

    #[test]
    fn missing_source_flag_is_distinct_from_qobuz_withdrawal() {
        let mut item = album("/music/Artist/Album");
        item.source_unavailable = true;
        let json = serde_json::to_value(item).expect("feed item serializes");
        assert_eq!(json["sourceUnavailable"], true);
        assert!(json.get("qobuzUnavailable").is_none());
    }

    fn unique_store_dir(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NONCE: AtomicU64 = AtomicU64::new(0);
        let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "qbz-qt-library-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    /// Logout must close the active SQLite handle, not erase that profile.
    /// A later guest entry reopens users/0, while an account activation in
    /// between sees only its own favourites.
    #[test]
    fn local_favorites_rebind_between_guest_and_account_profiles() {
        let guest = unique_store_dir("guest");
        let account = unique_store_dir("account");
        std::fs::create_dir_all(&guest).expect("guest dir");
        std::fs::create_dir_all(&account).expect("account dir");

        init_local_favorites(&guest);
        let snapshot = LocalFavItem {
            kind: "album".into(),
            id: "/music/Guest/Album".into(),
            title: "Guest Album".into(),
            subtitle: "Guest Artist".into(),
            artwork_url: String::new(),
            artist: "Guest Artist".into(),
            source: "local".into(),
            favorited_at: 0,
        };
        LOCAL_FAVS
            .lock()
            .expect("local favourites lock")
            .as_ref()
            .expect("guest store")
            .favorite(&snapshot)
            .expect("persist guest favourite");
        assert!(is_local_favorite("album", "/music/Guest/Album"));

        init_local_favorites(&account);
        assert!(!is_local_favorite("album", "/music/Guest/Album"));

        teardown();
        assert!(!is_local_favorite("album", "/music/Guest/Album"));

        init_local_favorites(&guest);
        assert!(is_local_favorite("album", "/music/Guest/Album"));

        teardown();
        let _ = std::fs::remove_dir_all(&guest);
        let _ = std::fs::remove_dir_all(&account);
    }

    // --- Artist page "In library" membership (#737 round, contract §3) ------

    fn owned(
        kind: &str,
        id: &str,
        group: &str,
        artist_id: &str,
        album_artist_id: &str,
    ) -> FeedItem {
        FeedItem {
            kind: kind.into(),
            group: group.into(),
            source: "qobuz".into(),
            id: id.into(),
            title: id.into(),
            artist_id: artist_id.into(),
            album_artist_id: album_artist_id.into(),
            is_favorite: group == "favorites",
            ..Default::default()
        }
    }

    /// A hearted track joins through EITHER id: the performer (feed
    /// `artist_id`) or the album's main artist — the owner's Megadeth rows
    /// whose performer was not the page's artist.
    #[test]
    fn artist_library_matches_performer_or_album_artist() {
        let feed = vec![
            owned("track", "1", "favorites", "42", ""),
            owned("track", "2", "favorites", "7", "42"),
            owned("track", "3", "favorites", "7", "7"),
            owned("album", "a", "favorites", "42", ""),
        ];
        let ids: Vec<String> = artist_library_items_from(&feed, "42")
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(ids, vec!["1", "2", "a"]);
    }

    /// Local / Plex rows and the `following` group never enter — the tab is
    /// `Library > Albums / Tracks`, not `Library > All`.
    #[test]
    fn artist_library_excludes_local_and_following() {
        let mut local = owned("track", "9", "local", "42", "");
        local.source = "local".into();
        let feed = vec![local, owned("artist", "42", "following", "42", "")];
        assert!(artist_library_items_from(&feed, "42").is_empty());
    }

    /// A purchased album that is also hearted collapses to ONE row — the
    /// purchase row (its quality badge survives) — carrying the heart; a
    /// purchase that is not hearted carries none.
    #[test]
    fn artist_library_dedups_on_the_purchase_row() {
        let feed = vec![
            owned("album", "a", "favorites", "42", ""),
            owned("album", "a", "purchases", "42", ""),
            owned("album", "b", "purchases", "42", ""),
        ];
        let rows = artist_library_items_from(&feed, "42");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "a");
        assert_eq!(rows[0].group, "purchases");
        assert!(rows[0].is_favorite);
        assert_eq!(rows[1].id, "b");
        assert!(!rows[1].is_favorite);
    }
}

#[cfg(test)]
mod playlist_deletion_regressions {
    use super::*;

    fn snapshot() -> LibraryData {
        let row = |id: &str, kind: &str, favorite: bool| FeedItem {
            id: id.into(),
            kind: kind.into(),
            source: "qobuz".into(),
            playlist_owned: kind == "playlist",
            is_favorite: favorite,
            ..Default::default()
        };
        LibraryData {
            feed: vec![
                row("42", "playlist", true),
                row("42", "playlist", true),
                row("43", "playlist", false),
                row("42", "album", true),
            ],
            counts: LibraryCounts {
                all: 4,
                playlists: 2,
                albums: 1,
                tracks: 0,
                artists: 0,
                labels: 0,
            },
        }
    }

    #[test]
    fn owned_delete_removes_all_copies_even_if_hearted() {
        let mut data = snapshot();
        let deleted = HashSet::from(["42".to_string()]);
        exclude_deleted_playlists(&mut data, &deleted);
        assert_eq!(data.feed.len(), 2);
        assert!(data.feed.iter().any(|r| r.kind == "album" && r.id == "42"));
        assert_eq!(data.counts.playlists, 1);
        assert_eq!(data.counts.all, 2);
        exclude_deleted_playlists(&mut data, &deleted);
        assert_eq!(
            data.counts.playlists, 1,
            "repeated reconciliation is idempotent"
        );
        assert_eq!(data.counts.all, 2);
    }

    #[test]
    fn late_list_response_cannot_resurrect_deleted_playlist() {
        let deleted = HashSet::from(["42".to_string(), "43".to_string()]);
        let mut late_response = snapshot();
        exclude_deleted_playlists(&mut late_response, &deleted);
        assert_eq!(late_response.counts.playlists, 0);
        assert_eq!(late_response.counts.all, 1);
        assert_eq!(late_response.feed[0].kind, "album");
    }

    #[test]
    fn no_successful_delete_keeps_snapshot_intact() {
        let mut data = snapshot();
        exclude_deleted_playlists(&mut data, &HashSet::new());
        assert_eq!(data.feed.len(), 4);
        assert_eq!(data.counts.playlists, 2);
    }
}
