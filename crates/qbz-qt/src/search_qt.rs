//! Search controller — the QML port of the Slint search surfaces
//! (crates/qbz/src/search.rs pure mappings + the main.rs SearchActions
//! handlers):
//!
//! - **Cortinilla** (live as-you-type dropdown): `live()` loads the combined
//!   search, persists the page into the intelligent-search cache (SWR), picks
//!   the learned top result, and publishes ONE JSON document
//!   (`cortinillaJson`) with the top-result + capped sections (albums 5 /
//!   artists 2 / tracks 3 / playlists 3, spec §6.2.3 order) and controller-
//!   assigned flat indices. Keyboard selection (`move_selection`) rebuilds
//!   the navigable order from the snapshot exactly like the Slint handler.
//! - **Results page** (`submit` / `search_all_action`): `search_all` mapped
//!   to the page document (tabs, totals, most-popular hero, carousel-dedupe),
//!   per-category `load_more` (PAGE_SIZE 20) and the searchType filter radios
//!   (MainArtist/Performer/Composer/Label/ReleaseName) re-querying the three
//!   filterable categories.
//! - **Intelligent Search service** (the 2.0.0 opt-out module): the same
//!   headless `qbz_app::settings::search_service::SearchService` the Slint
//!   wraps — bound per session (`init`, next to the pinned store), kill
//!   switch from `ui_prefs.intelligent_search`, `record` on row activation,
//!   `rank_within` before truncation, `top_for_query` for the promoted row.
//!   The RESULT cache (CAPA A) is deliberately not written: nothing has ever
//!   read it (see the note on the deleted `store` below).
//!
//! Notes:
//! - The LOCAL "on this device" sections ARE ported (`search_local.rs`):
//!   albums / artists / tracks, appended last, Plex unioned in, fetched
//!   CONCURRENTLY with the Qobuz half so an offline or slow Qobuz still
//!   yields local results. The results page also carries the same local sections.
//! - Artist "following" flags are resolved from `fav_cache_qt` (the ported
//!   favourite-id cache) — they used to be hard-`false`, which made a search
//!   hit on a followed artist draw "Follow" and un-follow them on click.
//!
//! Blacklist filtering is live on every one of this module's four fetch paths
//! (spec 03 §9.2 F8): `live` and `submit` hand the snapshot pair INTO
//! `core.search_all` (`search.rs:1045-1059`, `:1095-1115`), and `load_more` /
//! `filter_changed` post-filter their pages because the paged
//! `search_albums` / `search_tracks` / `search_artists` pass-throughs take no
//! snapshot (`search.rs:1616-1666`).

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::album_qt::format_album_title;
use cxx_qt_lib::QString;
pub(crate) use qbz_app::settings::search_service::InteractionAction;
use qbz_app::settings::search_service::SearchService;
use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use qbz_models::{Album, Artist, MostPopularItem, Playlist, SearchAllResults, Track};
use serde::Serialize;

// ---------------------------------------------------------------------------
// Intelligent Search service singleton (search_service.rs wrapper, 1:1)
// ---------------------------------------------------------------------------

static SERVICE: Mutex<Option<SearchService>> = Mutex::new(None);

/// Bind the per-user service (session activation, next to init_pinned).
/// `enabled` seeds from the persisted ui_prefs.intelligent_search pref.
pub fn init(base_dir: &Path, enabled: bool) {
    let service = SearchService::new(base_dir);
    service.set_enabled(enabled);
    if let Ok(mut guard) = SERVICE.lock() {
        *guard = Some(service);
    }
    log::info!("[qbz-qt] intelligent search service bound (enabled={enabled})");
}

fn with_service<T>(default: T, f: impl FnOnce(&SearchService) -> T) -> T {
    SERVICE
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(f))
        .unwrap_or(default)
}

fn with_service_mut(f: impl FnOnce(&mut SearchService)) {
    if let Ok(mut guard) = SERVICE.lock() {
        if let Some(service) = guard.as_mut() {
            f(service);
        }
    }
}

pub fn is_enabled() -> bool {
    with_service(false, |s| s.enabled())
}

/// Drop the per-user search service on logout.
///
/// Without this the NEXT account inherits the previous one's learned ranking:
/// `top_for_query` would promote a stranger's most-clicked result, and the
/// interaction store would keep accumulating under their bucket. The reference
/// tears it down for the same reason.
///
/// The version counters and payload snapshots are cleared with it, so a load
/// still in flight at logout cannot publish into the next session.
pub fn teardown() {
    if let Ok(mut guard) = SERVICE.lock() {
        *guard = None;
    }
    crate::search_cache_qt::teardown();
    next_page_version();
    *PAGE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    crate::search_bridge::ui(|mut b| {
        if PAGE.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
            b.as_mut().set_search_json(QString::from("{}"));
        }
    });
    next_cort_version();
    *LAST_CORT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *LAST_CORT_LOCAL.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
}

pub fn set_enabled(on: bool) {
    with_service((), |s| s.set_enabled(on));
}

// `store` — DELETED. It persisted into the Intelligent Search result cache
// (CAPA A), which NOTHING has ever read: `SearchService::cached` has zero
// non-test callers in either frontend, and `git log -S` shows the read side
// was never wired in any commit. So every keystroke paid a full serialize +
// fs::write of a multi-megabyte artist store, on the calling thread, inside
// the mutex the UI thread contends for — to feed a file nobody opens.
// Removing the writer cannot change any rendered output.
//
// The same deletion on the Slint side is commit S1a on branch S; this is the
// Qt half, landed here so the Qt binary stops paying it immediately instead
// of waiting for a merge.

fn record(query: &str, kind: &str, id: &str, action: InteractionAction) {
    with_service_mut(|s| s.record_interaction(query, kind, id, action));
}

fn top_for_query(query: &str) -> Option<(String, String)> {
    with_service(None, |s| s.top_for_query(query))
}

fn rank_within<T>(query: &str, kind: &str, items: &mut Vec<T>, id_of: impl Fn(&T) -> String) {
    with_service((), |s| s.rank_within(query, kind, items, id_of));
}

// ---------------------------------------------------------------------------
// ui_prefs intelligent_search (the 2.0.0 opt-out).
//
// Both halves are settings_qt's now. This module used to keep its own path,
// its own `json!({})` fallback and its own truncating `std::fs::write` on
// ui_prefs.json — the file the SHIPPING Slint build has open — so one search
// toggle could hand Slint's `load()` an empty document and let its next save
// flatten the whole profile. See the write-discipline block in settings_qt.rs.
// ---------------------------------------------------------------------------

pub fn intelligent_search_pref() -> bool {
    crate::settings_qt::pref_bool("intelligent_search", true)
}

// `toggle_intelligent_search` lived here and was DELETED (contract DEAD-1):
// its only caller was a bridge invokable with zero QML callers, so the whole
// chain was unreachable. The live path is the Settings > Appearance row,
// which goes through `settings_qt`'s key dispatch to `set_enabled` below.
// Its torn-read guard is not lost — `settings_qt::toggle_pref_bool` still
// owns that behaviour for every other toggle row.

// ---------------------------------------------------------------------------
// Row types (search.rs AlbumRow / TrackRowData / ArtistRow / PlaylistRow,
// shaped so the QML cards (AlbumCard etc.) can consume them directly)
// ---------------------------------------------------------------------------

#[derive(Clone, Default, Serialize)]
pub struct CardRow {
    pub id: String,
    pub title: String,
    pub artist: String,
    #[serde(rename = "artistId")]
    pub artist_id: String,
    pub genre: String,
    pub year: String,
    #[serde(rename = "qualityTier")]
    pub quality_tier: String,
    #[serde(rename = "qualityLabel")]
    pub quality_label: String,
    #[serde(rename = "artUrl")]
    pub art_url: String,
    #[serde(rename = "artPath")]
    pub art_path: String,
    #[serde(rename = "isFavorite")]
    pub is_favorite: bool,
    #[serde(rename = "isPinned")]
    pub is_pinned: bool,
}

#[derive(Clone, Default, Serialize)]
pub struct TrackRow {
    /// Featured performers already joined ("X, Y"); "" when none.
    #[serde(rename = "featured")]
    pub featured: String,
    pub id: String,
    pub title: String,
    pub artist: String,
    #[serde(rename = "artistId")]
    pub artist_id: String,
    /// The album's TITLE. `album_id` alone is not enough for the row's album
    /// column: TrackRow.qml draws `item.album` as the label and uses
    /// `item.albumId` for the click, so without this the column renders empty
    /// and the only way to the album is the context menu.
    pub album: String,
    #[serde(rename = "albumId")]
    pub album_id: String,
    pub duration: String,
    #[serde(rename = "qualityTier")]
    pub quality_tier: String,
    #[serde(rename = "qualityLabel")]
    pub quality_label: String,
    #[serde(rename = "qualityDetail")]
    pub quality_detail: String,
    pub explicit: bool,
    #[serde(rename = "artUrl")]
    pub art_url: String,
    #[serde(rename = "artPath")]
    pub art_path: String,
    #[serde(rename = "isFavorite")]
    pub is_favorite: bool,
    /// Qobuz PULLED this track (`streamable: false` — contract §5.1). Search is
    /// the one surface the Tauri reference never treated (§4.2) and D4 does not
    /// exempt it: a dead row here is as clickable and as silent as anywhere
    /// else. Search payloads DO carry the flat key (all 10 in
    /// `qobuz-api/search-results-response.json`).
    #[serde(
        default,
        rename = "qobuzUnavailable",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub not_streamable: bool,
    /// Refines `qobuzUnavailable`: a pre-release rather than a pulled track
    /// (`qbz_models::Track::is_upcoming`).
    #[serde(
        default,
        rename = "qobuzUpcoming",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub upcoming: bool,
}

#[derive(Clone, Default, Serialize)]
pub struct ArtistRow {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    #[serde(rename = "artUrl")]
    pub art_url: String,
    #[serde(rename = "artPath")]
    pub art_path: String,
    pub following: bool,
    /// Pin badge state at build time (`CardRow` carries the album twin).
    /// ArtistCard draws the same glyph as the album card, and a row that
    /// never carries the flag makes it lie — the first click on an
    /// already-pinned artist UN-pins it.
    #[serde(rename = "isPinned")]
    pub is_pinned: bool,
}

#[derive(Clone, Default, Serialize)]
pub struct PlaylistRow {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    #[serde(rename = "artUrl")]
    pub art_url: String,
    #[serde(rename = "artPath")]
    pub art_path: String,
    /// The ownership tri-state, under the names `cards/PlaylistCard.qml`
    /// actually reads (`item.playlistOwned` / `item.playlistFollowing`).
    ///
    /// They used to serialize as `isOwned` / `isFollowing`: names NOTHING in
    /// the tree consumed (verified by grep over `qml/` — the only
    /// `isFollowing` readers are LabelView's label doc, PlaylistView's
    /// playlist doc and ArtistView's artist doc, three different structs). So
    /// the fields were on the wire, and the card still saw `undefined` on both
    /// and fell into the "foreign playlist, offer Follow" arm. `library_qt::
    /// FeedItem` and `home_qt::HomeCard` already publish these two spellings;
    /// one card must read one contract on every surface.
    #[serde(rename = "playlistOwned")]
    pub is_owned: bool,
    #[serde(rename = "playlistFollowing")]
    pub is_following: bool,
    /// The library heart — only drawn on the OWNED arm of the tri-state.
    #[serde(rename = "isFavorite")]
    pub is_favorite: bool,
    /// Pin badge state at build time — see `ArtistRow::is_pinned`.
    #[serde(rename = "isPinned")]
    pub is_pinned: bool,
}

// ---------------------------------------------------------------------------
// Pure helpers (search.rs: tier / quality_label / mmss / year_of /
// format_album_title)
// ---------------------------------------------------------------------------

fn tier(bit_depth: Option<u32>) -> &'static str {
    match bit_depth {
        Some(depth) if depth >= 24 => "hires",
        Some(_) => "cd",
        None => "",
    }
}

fn quality_label(bit_depth: Option<u32>, sample_rate: Option<f64>) -> String {
    match bit_depth {
        None => String::new(),
        Some(depth) => {
            let prefix = if depth >= 24 { "Hi-Res" } else { "CD" };
            let rate = sample_rate.unwrap_or(if depth >= 24 { 96.0 } else { 44.1 });
            let rate = if rate.fract().abs() < f64::EPSILON {
                format!("{}", rate as i64)
            } else {
                format!("{rate}")
            };
            format!("{prefix} {depth}-bit / {rate} kHz")
        }
    }
}

fn mmss(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn year_of(date: Option<&str>) -> String {
    date.and_then(|d| d.get(0..4)).unwrap_or("").to_string()
}

fn map_album(album: &Album) -> CardRow {
    CardRow {
        // Pin badge state from the per-user store (search.rs stamps it the
        // same way). The field existed on the wire but nothing ever filled
        // it, so every search card drew the hollow glyph and the first
        // click on an already-pinned album UN-pinned it.
        is_pinned: crate::sidebar_qt::is_pinned("album", &album.id),
        // Heart at build time. `CardRow` has always DECLARED `is_favorite`
        // (line 158) and nothing ever filled it, so `SearchView.qml` mounts
        // AlbumCard with `isFavorite: false`: a favourited album drew hollow
        // and, now that the toggle takes its direction from the populated
        // cache, the first click REMOVED it from the library.
        is_favorite: crate::fav_cache_qt::is_album_favorite(&album.id),
        id: album.id.clone(),
        title: format_album_title(&album.title, album.version.as_deref()),
        artist: album.artist.name.clone(),
        artist_id: album.artist.id.to_string(),
        genre: album
            .genre
            .clone()
            .map(|g| g.name)
            .filter(|n| !n.is_empty())
            .unwrap_or_default(),
        // BOTH shapes, through the shared readers. `search_albums` returns the
        // very same `SearchResultsPage<Album>` that `/award/getAlbums` does,
        // and that endpoint answers with the NESTED fields only — a flat-only
        // read there produced cards with no date and no badge (owner smoke on
        // the award grid, 2026-08-16). Same container, same exposure.
        year: year_of(crate::home_qt::album_release_date(album).as_deref()),
        quality_tier: crate::home_qt::album_quality_tier(
            album,
            crate::home_qt::album_audio_parts(album).0,
        )
        .to_string(),
        quality_label: {
            let (bd, sr) = crate::home_qt::album_audio_parts(album);
            quality_label(bd, sr)
        },
        art_url: crate::cover_artwork_qt::prefer_album_cover(
            &album.id,
            // Search-result album grid card: full variant (best()) — the
            // down-tier was reverted after the 2026-08-15 owner smoke
            // (contract 04 §3).
            album.image.best().cloned().unwrap_or_default(),
        ),
        ..Default::default()
    }
}

fn map_track(track: &Track) -> TrackRow {
    let mut title = track.title.clone();
    if let Some(version) = track.version.as_ref().filter(|v| !v.is_empty()) {
        title = format!("{title} ({version})");
    }
    // Track row art: full variant (best()) — the thumbnail down-tier was reverted after the 2026-08-15 owner smoke (contract 04 §3).
    let artwork_url = track
        .album
        .as_ref()
        .and_then(|a| a.image.best().cloned())
        .unwrap_or_default();
    let album_id = track
        .album
        .as_ref()
        .map(|a| a.id.clone())
        .unwrap_or_default();
    let album = track
        .album
        .as_ref()
        .map(|a| a.title.clone())
        .unwrap_or_default();
    let (artist, artist_id) = track
        .performer
        .clone()
        .map(|p| (p.name, p.id.to_string()))
        .unwrap_or_default();
    TrackRow {
        featured: crate::album_qt::featured_join(track.performers.as_deref(), &artist, &track.title),
        // Same story as `map_album`: the field was declared (line 186) and
        // never stamped, so every search track row drew the empty heart and
        // the first click sent `favorite/delete` on a track the user had
        // favourited.
        is_favorite: crate::fav_cache_qt::contains_track(track.id),
        id: track.id.to_string(),
        title,
        artist,
        artist_id,
        album,
        album_id,
        duration: mmss(track.duration),
        quality_tier: tier(track.maximum_bit_depth).to_string(),
        quality_label: quality_label(track.maximum_bit_depth, track.maximum_sampling_rate),
        quality_detail: crate::home_qt::quality_detail_from_parts(
            track.maximum_bit_depth,
            track.maximum_sampling_rate,
        ),
        explicit: track.parental_warning,
        art_url: artwork_url,
        // §5.1 via §3.1's single interpreter of absence.
        not_streamable: !track.is_streamable(),
        upcoming: track.is_upcoming(),
        ..Default::default()
    }
}

/// `following` is DERIVED, not passed in. Every call site handed it `false`
/// (the module's POC-NOTE said "no favorites snapshot is consulted"), which
/// meant a search hit on an artist the user follows drew "Follow" and the
/// click un-followed them. The snapshot exists now — `fav_cache_qt` — and the
/// reference does exactly this (`search.rs::map_most_popular` resolves the
/// same flag from its `favorite_artists` set), so the parameter is gone rather
/// than left as a lie every caller has to remember not to tell.
fn map_artist(artist: &Artist) -> ArtistRow {
    let following = crate::fav_cache_qt::is_artist_favorite(artist.id);
    ArtistRow {
        is_pinned: crate::sidebar_qt::is_pinned("artist", &artist.id.to_string()),
        id: artist.id.to_string(),
        title: artist.name.clone(),
        subtitle: match artist.albums_count {
            Some(n) if n > 0 => qbz_i18n::tf("{} album", "{} albums", n as i64, &[&n.to_string()]),
            _ => String::new(),
        },
        // ArtistCard grid cell (200px): full variant (best()) — the down-tier
        // was reverted after the 2026-08-15 owner smoke (contract 04 §3).
        // Prefer the user's custom portrait override (keyed by name).
        art_url: crate::cover_artwork_qt::prefer_artist_image(
            &artist.name,
            artist
                .image
                .as_ref()
                .and_then(|i| i.best().cloned())
                .unwrap_or_default(),
        ),
        art_path: String::new(),
        following,
    }
}

fn map_playlist(playlist: &Playlist) -> PlaylistRow {
    // Highest-resolution non-empty cover list wins (playlist_cover_urls).
    let cover = playlist
        .images300
        .as_ref()
        .filter(|v| !v.is_empty())
        .or(playlist.images150.as_ref().filter(|v| !v.is_empty()))
        .or(playlist.images.as_ref().filter(|v| !v.is_empty()))
        .and_then(|v| v.first().cloned())
        .unwrap_or_default();
    let mut subtitle = playlist.owner.name.clone();
    if playlist.tracks_count > 0 {
        let count = playlist.tracks_count;
        let tracks_label =
            qbz_i18n::tf("{} track", "{} tracks", count as i64, &[&count.to_string()]);
        subtitle = if subtitle.is_empty() {
            tracks_label
        } else {
            format!("{subtitle}   •   {tracks_label}")
        };
    }
    PlaylistRow {
        is_pinned: crate::sidebar_qt::is_pinned("playlist", &playlist.id.to_string()),
        // The overlay tri-state. `is_owned` is authoritative from the owner id
        // — that is exactly what the reference does (search.rs:367,
        // `current_user_id() == playlist.owner.id`) and it is the half that
        // matters most: without it a search hit on the user's OWN playlist
        // offered "Follow on Qobuz" and the click subscribed them to
        // themselves. `is_following` is only knowable from the user's own
        // playlist list, so it comes from the ownership snapshot (the
        // reference leaves it `false` here; this is a strict improvement and
        // degrades to `false` before the first snapshot).
        is_owned: crate::playlist_qt::owns(playlist.owner.id),
        is_following: crate::playlist_qt::is_following(playlist.id),
        // The heart is the qbz-local library.db flag, mirrored in the cache.
        // PlaylistCard only draws it on the OWNED arm, which is precisely why
        // it has to be stamped together with `is_owned` and not before it.
        is_favorite: crate::fav_cache_qt::is_playlist_favorite(playlist.id),
        id: playlist.id.to_string(),
        title: playlist.name.clone(),
        subtitle,
        art_url: cover,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Blacklist snapshots (spec 03 §9.2 F8)
// ---------------------------------------------------------------------------

/// The `(artists, albums)` blacklist snapshot pair every fetch path in this
/// module needs, in the reference's exact shape: **one** shared `is_enabled()`
/// gate for both axes, empty sets when the feature is off so the `qbz_core`
/// predicates short-circuit on `bl.is_empty() && album_bl.is_empty()`
/// (`search.rs:1045-1055` / `:1618-1626`, `qbz-core/src/core.rs:128`).
///
/// Snapshotted per fetch, not cached: the store is mutated from the manager,
/// the artist page and the album page with no change-notify, so a stale
/// snapshot is exactly the leak this closes.
fn blacklist_snapshots() -> (HashSet<u64>, HashSet<String>) {
    if crate::artist_blacklist::is_enabled() {
        (
            crate::artist_blacklist::ids_snapshot(),
            crate::artist_blacklist::album_ids_snapshot(),
        )
    } else {
        Default::default()
    }
}

// ---------------------------------------------------------------------------
// Artwork (the reload_home pattern: disk hits inline, one background
// download + republish)
// ---------------------------------------------------------------------------

/// Resolve cover URLs into the row's art_path from the disk cache; returns
/// the urls still missing (deduped). The slot pair is (source url, art_path)
/// — the path starts empty and is filled on a cache hit (artwork_qt's
/// attach_cached pattern for Home).
fn attach_urls(pairs: Vec<(String, &mut String)>) -> Vec<String> {
    let mut missing = Vec::new();
    for (url, slot) in pairs {
        if url.is_empty() {
            continue;
        }
        let hit = crate::artwork_qt::cached_path(&url);
        if hit.is_empty() {
            if !missing.contains(&url) {
                missing.push(url);
            }
        } else {
            *slot = hit;
        }
    }
    missing
}

// ---------------------------------------------------------------------------
// Cortinilla payload (search.rs CortRow / CortSection / CortinillaData)
// ---------------------------------------------------------------------------

#[derive(Clone, Default, Serialize, PartialEq)]
pub struct CortRow {
    pub kind: String,
    pub id: String,
    pub source: String,
    pub title: String,
    pub subtitle: String,
    /// Bare exact quality for the compact third row ("24-bit / 96 kHz").
    /// Empty for entity kinds without an audio-quality contract.
    #[serde(rename = "qualityDetail")]
    pub quality_detail: String,
    #[serde(rename = "artUrl")]
    pub art_url: String,
    #[serde(rename = "artPath")]
    pub art_path: String,
    /// TRACK rows only ("Open containing album"): the containing album — a
    /// Qobuz album id or a local album group key (both are what
    /// `crate::open_album` routes). Empty when unknown; the QML menu gates
    /// its entry on that.
    #[serde(rename = "albumId")]
    pub album_id: String,
    #[serde(rename = "flatIndex")]
    pub flat_index: i32,
}

#[derive(Clone, Default, Serialize, PartialEq)]
pub struct CortSection {
    pub title: String,
    pub kind: String,
    #[serde(rename = "hasMore")]
    pub has_more: bool,
    pub rows: Vec<CortRow>,
}

#[derive(Clone, Default, Serialize, PartialEq)]
pub struct CortinillaData {
    pub query: String,
    pub top: Option<CortRow>,
    pub sections: Vec<CortSection>,
}

const CORTINILLA_CAP_ALBUMS: usize = 5;
const CORTINILLA_CAP_ARTISTS: usize = 2;
const CORTINILLA_CAP_TRACKS: usize = 3;
const CORTINILLA_CAP_PLAYLISTS: usize = 3;

/// Make a duplicated Top result consume the same canonical row as its visible
/// section entry.
///
/// Qobuz's `most_popular` object can be a shallower projection than the same
/// item in `tracks.items` / `albums.items` (notably, a Track may omit its album
/// image). Keeping both projections produced two independent cache keys on
/// screen: the section row had a cover while Top result stayed blank until a
/// later page load happened to hydrate it. Identity is `(kind, id)`; when that
/// identity is already visible, replace the hero wholesale so artwork,
/// quality and any future fields cannot drift independently.
fn canonicalize_top_result(top: &mut Option<CortRow>, visible: &[&[CortRow]]) {
    let Some((kind, id)) = top.as_ref().map(|row| (row.kind.as_str(), row.id.as_str())) else {
        return;
    };
    let replacement = visible
        .iter()
        .flat_map(|rows| rows.iter())
        .find(|row| row.kind == kind && row.id == id)
        .cloned();
    if let Some(row) = replacement {
        *top = Some(row);
    }
}

/// search.rs `map_search_all_to_cortinilla`, 1:1 (ranking, caps, top-result
/// selection, flat-index assignment).
fn map_search_all_to_cortinilla(query: &str, results: &SearchAllResults) -> CortinillaData {
    let to_artist_row = |a: &Artist| CortRow {
        kind: "artist".into(),
        id: a.id.to_string(),
        source: "qobuz".into(),
        title: a.name.clone(),
        subtitle: map_artist(a).subtitle,
        // Cortinilla dropdown row art (~40px): full variant (best()) — the
        // thumbnail down-tier was reverted after the 2026-08-15 owner smoke
        // (contract 04 §3).
        art_url: a
            .image
            .as_ref()
            .and_then(|i| i.best().cloned())
            .unwrap_or_default(),
        ..Default::default()
    };
    let to_album_row = |al: &Album| {
        let m = map_album(al);
        let (bit_depth, sample_rate) = crate::home_qt::album_audio_parts(al);
        let quality_detail = if bit_depth.is_some() && sample_rate.is_some() {
            crate::home_qt::quality_detail_from_parts(bit_depth, sample_rate)
        } else {
            String::new()
        };
        CortRow {
            kind: "album".into(),
            id: m.id,
            source: "qobuz".into(),
            title: m.title,
            subtitle: m.artist,
            quality_detail,
            art_url: m.art_url,
            ..Default::default()
        }
    };
    let to_track_row = |t: &Track| {
        let m = map_track(t);
        let quality_detail = if t.maximum_bit_depth.is_some() && t.maximum_sampling_rate.is_some() {
            m.quality_detail.clone()
        } else {
            String::new()
        };
        CortRow {
            kind: "track".into(),
            id: m.id,
            source: "qobuz".into(),
            title: m.title,
            subtitle: m.artist,
            quality_detail,
            art_url: m.art_url,
            album_id: m.album_id,
            ..Default::default()
        }
    };
    let to_playlist_row = |p: &Playlist| {
        let m = map_playlist(p);
        CortRow {
            kind: "playlist".into(),
            id: m.id,
            source: "qobuz".into(),
            title: m.title,
            subtitle: m.subtitle,
            art_url: m.art_url,
            ..Default::default()
        }
    };

    let mut artist_rows: Vec<CortRow> = results.artists.items.iter().map(&to_artist_row).collect();
    let mut album_rows: Vec<CortRow> = results.albums.items.iter().map(&to_album_row).collect();
    let mut track_rows: Vec<CortRow> = results.tracks.items.iter().map(&to_track_row).collect();
    let mut playlist_rows: Vec<CortRow> = results
        .playlists
        .items
        .iter()
        .map(&to_playlist_row)
        .collect();

    rank_within(query, "artist", &mut artist_rows, |r| r.id.clone());
    rank_within(query, "album", &mut album_rows, |r| r.id.clone());
    rank_within(query, "track", &mut track_rows, |r| r.id.clone());
    rank_within(query, "playlist", &mut playlist_rows, |r| r.id.clone());
    artist_rows.truncate(CORTINILLA_CAP_ARTISTS);
    album_rows.truncate(CORTINILLA_CAP_ALBUMS);
    track_rows.truncate(CORTINILLA_CAP_TRACKS);
    playlist_rows.truncate(CORTINILLA_CAP_PLAYLISTS);

    // Top result: the learned (kind, id) for this query, else the
    // most_popular hero, else first artist, else first album.
    let top_kind_id = top_for_query(query);
    let find_in = |kind: &str, id: &str| -> Option<CortRow> {
        let sect = match kind {
            "artist" => &artist_rows,
            "album" => &album_rows,
            "track" => &track_rows,
            "playlist" => &playlist_rows,
            _ => return None,
        };
        sect.iter().find(|r| r.id == id).cloned()
    };
    let mut top: Option<CortRow> = top_kind_id
        .and_then(|(kind, id)| {
            find_in(&kind, &id).or_else(|| match kind.as_str() {
                "artist" => results
                    .artists
                    .items
                    .iter()
                    .find(|a| a.id.to_string() == id)
                    .map(&to_artist_row),
                "album" => results
                    .albums
                    .items
                    .iter()
                    .find(|a| a.id == id)
                    .map(&to_album_row),
                "track" => results
                    .tracks
                    .items
                    .iter()
                    .find(|t| t.id.to_string() == id)
                    .map(&to_track_row),
                "playlist" => results
                    .playlists
                    .items
                    .iter()
                    .find(|p| p.id.to_string() == id)
                    .map(&to_playlist_row),
                _ => None,
            })
        })
        .or_else(|| match &results.most_popular {
            Some(MostPopularItem::Artists(a)) => Some(to_artist_row(a)),
            Some(MostPopularItem::Albums(a)) => Some(to_album_row(a)),
            Some(MostPopularItem::Tracks(t)) => Some(to_track_row(t)),
            None => None,
        })
        .or_else(|| artist_rows.first().cloned())
        .or_else(|| album_rows.first().cloned());
    canonicalize_top_result(
        &mut top,
        &[&artist_rows, &album_rows, &track_rows, &playlist_rows],
    );

    let mut sections: Vec<CortSection> = Vec::new();
    let mut push_section = |title: &str, kind: &str, rows: Vec<CortRow>, total: u32| {
        if !rows.is_empty() {
            sections.push(CortSection {
                title: title.to_string(),
                kind: kind.to_string(),
                has_more: total as usize > rows.len(),
                rows,
            });
        }
    };
    push_section(
        &qbz_i18n::t("Albums"),
        "album",
        album_rows,
        results.albums.total,
    );
    push_section(
        &qbz_i18n::t("Artists"),
        "artist",
        artist_rows,
        results.artists.total,
    );
    push_section(
        &qbz_i18n::t("Tracks"),
        "track",
        track_rows,
        results.tracks.total,
    );
    push_section(
        &qbz_i18n::t("Playlists"),
        "playlist",
        playlist_rows,
        results.playlists.total,
    );

    let mut data = CortinillaData {
        query: query.to_string(),
        top,
        sections,
    };
    assign_flat_indices(&mut data);
    data
}

pub(crate) fn assign_flat_indices(data: &mut CortinillaData) {
    let mut next = 0;
    if let Some(top) = &mut data.top {
        top.flat_index = next;
        next += 1;
    } else {
        next = 1; // no top result: section rows start at 1 (Slint convention)
    }
    for section in &mut data.sections {
        for row in &mut section.rows {
            row.flat_index = next;
            next += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Cortinilla controller state
// ---------------------------------------------------------------------------

/// Every lock on this takes `unwrap_or_else(|e| e.into_inner())`, not
/// `unwrap()`: `row_clicked` and `move_selection` run on the Qt GUI thread
/// (they are invokable bodies), so a poisoned lock there would take the whole
/// UI down. The guarded value is a plain snapshot — a panic mid-write leaves
/// it stale at worst, never inconsistent.
static LAST_CORT: Mutex<Option<CortinillaData>> = Mutex::new(None);

/// The RAW local rows behind the payload above, written in lockstep with it.
///
/// The click router needs the concrete `LocalTrack` to play, and it cannot
/// re-resolve one from the row id: a local ARTIST row has no id at all, and a
/// local ALBUM row's id is a group key, not a track. So the fetched vector is
/// kept and the router indexes into it. Same poison-tolerant lock discipline
/// as `LAST_CORT` — `row_clicked` runs on the Qt GUI thread.
static LAST_CORT_LOCAL: Mutex<Vec<qbz_library::LocalTrack>> = Mutex::new(Vec::new());
static CORT_VERSION: AtomicU64 = AtomicU64::new(0);
/// The keyboard-selected flat index mirrored from the bridge property (so
/// `move_selection` needs no QML round-trip).
static CURRENT_SEL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

fn next_cort_version() -> u64 {
    CORT_VERSION.fetch_add(1, Ordering::SeqCst) + 1
}
fn is_current_cort_version(v: u64) -> bool {
    CORT_VERSION.load(Ordering::SeqCst) == v
}

fn set_cortinilla_open(open: bool) {
    CORT_OPEN.store(open, Ordering::SeqCst);
    crate::search_bridge::ui(move |mut b| {
        b.as_mut().set_cortinilla_open(open);
        if !open {
            b.as_mut().set_cortinilla_selected_index(-1);
        }
    });
}

/// Rust-side mirror of `QbzBridge.cortinillaOpen` (the property itself lives
/// on ANOTHER singleton, unreachable from hotkeys_bridge). Every write already
/// funnels through `set_cortinilla_open` above — QML never writes the property
/// directly (grep `cortinillaOpen =` in qml/: no hits) — so the mirror is
/// exact. Read by the hotkeys pipeline's (B) dropdown-steal and the §1.2
/// Escape stack (2026-08-03 hotkeys-port contract §1.1/§1.2).
static CORT_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn cortinilla_open() -> bool {
    CORT_OPEN.load(Ordering::SeqCst)
}

fn set_selected(index: i32, scroll_y: f64) {
    CURRENT_SEL.store(index, Ordering::SeqCst);
    crate::search_bridge::ui(move |mut b| {
        b.as_mut().set_cortinilla_selected_index(index);
        b.as_mut().set_cortinilla_scroll_y(scroll_y);
    });
}

/// The live header query changed — called on EVERY keystroke (>= 2 chars),
/// not on a QML timer.
///
/// Two halves, and the split is load-bearing:
///
/// - **Synchronous half** — everything up to the sleep below. It runs on the
///   first poll of the spawned task, i.e. at the keystroke: gates, selection
///   reset, open, version bump, loading flag. This is what makes the panel and
///   its skeleton appear while the user is still typing, which is the
///   reference's behaviour (`qbz/src/main.rs:9641-9648` opens and raises
///   before starting the timer at `:9673`).
/// - **Debounced half** — after a 220 ms sleep (CORTINILLA_DEBOUNCE). A newer
///   keystroke has bumped the version by then, so the superseded task exits
///   without loading. Same shape as the immersive arm (`imm_live`).
///
/// `cortinillaLoading` has exactly ONE owner in the synchronous half and is
/// written exactly once more at the publish. Never in both halves for the
/// same keystroke.
pub async fn live(runtime: &Arc<AppRuntime<LoggingAdapter>>, query: &str) {
    let q = query.trim().to_string();
    if q.chars().count() < 2 {
        set_cortinilla_open(false);
        next_cort_version();
        return;
    }
    if !is_enabled() {
        // Module OFF is a SWAP, not a removal: the reference live-navigates
        // to the full results page on a 300 ms debounce instead of showing a
        // dropdown. Returning here made the kill switch remove a capability
        // rather than exchange it — typing did nothing at all until Enter.
        let version = next_cort_version();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        if !is_current_cort_version(version) {
            return;
        }
        submit(runtime, &q, None).await;
        return;
    }
    set_selected(-1, 0.0);
    set_cortinilla_open(true);
    let version = next_cort_version();
    // Offline OR an unauthenticated session widens the on-device caps: with
    // no Qobuz half the dropdown IS the local library, so a compact 3/2/3
    // block would waste the panel. Read on the synchronous side, before the
    // debounce, so it describes the session at the keystroke.
    let status = crate::offline_fwd::engine().status();
    let expand_local = status.is_offline() || status.offline_session;
    let caps = crate::search_local::LocalCaps::for_session(expand_local);

    // ---- the instant-paint probe (rulings R1 + R6) ----------------------
    // Synchronous, before the debounce, so what the user sees at t~0 is
    // decided here and cannot race the 220 ms sleep below.
    //
    // `cortinillaLoading` has exactly ONE owner: this block. On a HIT the
    // cached rows and `loading = false` go out in the SAME ui() closure, so
    // the skeleton never appears; on a miss the skeleton is raised. It is
    // written once more at the publish, and nowhere else.
    let shown: Option<CortinillaData> = crate::search_cache_qt::get(&q);
    match &shown {
        Some(cached) => {
            let json = serde_json::to_string(cached).unwrap_or_else(|_| "{}".into());
            crate::search_bridge::ui(move |mut b| {
                b.as_mut().set_cortinilla_json(QString::from(json.as_str()));
                b.as_mut().set_cortinilla_loading(false);
            });
        }
        None => {
            crate::search_bridge::ui(move |mut b| {
                b.as_mut().set_cortinilla_loading(true);
            });
        }
    }

    // ---- end of the synchronous half; the debounce starts here ----------
    // 220 ms (CORTINILLA_DEBOUNCE): one load per pause, not one per
    // keystroke. A newer keystroke bumps CORT_VERSION while this sleeps, so
    // the superseded task wakes, fails the guard and exits without touching
    // the network — the same idiom `imm_live` uses.
    tokio::time::sleep(std::time::Duration::from_millis(220)).await;
    if !is_current_cort_version(version) {
        return;
    }

    let t = std::time::Instant::now();
    // Blacklist filtering happens INSIDE search_all.
    let (bl, abl) = blacklist_snapshots();
    // CONCURRENT: the on-device query must not wait on the network, and the
    // network must not wait on SQLite. A slow or unreachable Qobuz still
    // yields a local-only dropdown, which is the whole point offline.
    let (results, local_rows) = tokio::join!(
        runtime.core().search_all(&q, &bl, &abl),
        crate::search_local::load_cortinilla_local(q.clone(), caps.fetch_limit(), true),
    );
    // A Qobuz failure DEGRADES to a local-only payload instead of discarding
    // everything — the reference's loader has no `?` and cannot return Err.
    // Returning here (which is what this used to do) is what made the whole
    // dropdown disappear the moment the network hiccuped.
    let mut data = match results {
        Ok(r) => {
            log::info!(
                "[qbz-qt][perf] cortinilla query \"{q}\" -> results: {:?}",
                t.elapsed(),
            );
            map_search_all_to_cortinilla(&q, &r)
        }
        Err(e) => {
            log::warn!("[qbz-qt] cortinilla: Qobuz half failed ({e}) — local-only payload");
            CortinillaData {
                query: q.clone(),
                top: None,
                sections: Vec::new(),
            }
        }
    };
    // The on-device sections go LAST, after every Qobuz category, and the
    // append re-runs assign_flat_indices so their rows get contiguous flat
    // indices after the Qobuz ones.
    crate::search_local::append_local_sections(&mut data, &local_rows, caps, &q);
    // The RAW rows, kept in lockstep with the payload: the click router plays
    // the concrete LocalTrack from this snapshot rather than re-resolving an
    // id, because local artist rows have no id at all.
    *LAST_CORT_LOCAL.lock().unwrap_or_else(|e| e.into_inner()) = local_rows;
    // Artwork: disk hits inline; misses download in the background and
    // republish the SAME payload (version-guarded).
    let mut urls: Vec<(String, &mut String)> = Vec::new();
    if let Some(top) = &mut data.top {
        urls.push((top.art_url.clone(), &mut top.art_path));
    }
    for section in &mut data.sections {
        for row in &mut section.rows {
            urls.push((row.art_url.clone(), &mut row.art_path));
        }
    }
    let missing = attach_urls(urls);
    *LAST_CORT.lock().unwrap_or_else(|e| e.into_inner()) = Some(data.clone());
    // WRITE POINT 1 of exactly two — AFTER pass-1 artwork resolution. Writing
    // any earlier caches rows with empty art_path, and the QML draws art_path
    // only: a hit would paint cover-less rows AND the gate below would fire on
    // every hit, which is the repaint this whole thing exists to avoid.
    crate::search_cache_qt::put(&q, &data);
    // THE EQUALITY GATE. If the instant paint already showed this exact
    // payload, there is nothing to repaint — and NOT repainting is the whole
    // difference between this and the version that glitched. `loading` is
    // still cleared, because a cache miss raised the skeleton.
    let unchanged = shown.as_ref() == Some(&data);
    if unchanged {
        log::debug!("[qbz-qt] cortinilla: fresh payload == instant paint, no repaint");
    }
    crate::search_bridge::ui(move |mut b| {
        if is_current_cort_version(version) {
            if !unchanged {
                let json = serde_json::to_string(&data).unwrap_or_else(|_| "{}".into());
                b.as_mut().set_cortinilla_json(QString::from(json.as_str()));
            }
            b.as_mut().set_cortinilla_loading(false);
        }
    });
    // The query is moved into the artwork task so write point 2 can key the
    // refreshed entry the same way write point 1 did.
    let q_art = q.clone();
    if !missing.is_empty() {
        crate::spawn(async move {
            crate::artwork_qt::download_missing(missing).await;
            if !is_current_cort_version(version) {
                return;
            }
            let mut data = LAST_CORT.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(data) = &mut data {
                let mut urls: Vec<(String, &mut String)> = Vec::new();
                if let Some(top) = &mut data.top {
                    urls.push((top.art_url.clone(), &mut top.art_path));
                }
                for section in &mut data.sections {
                    for row in &mut section.rows {
                        urls.push((row.art_url.clone(), &mut row.art_path));
                    }
                }
                let _ = attach_urls(urls);
                // WRITE POINT 2 of two: refresh the entry with the downloaded
                // covers. Without this the NEXT hit would instant-paint the
                // pass-1 payload, whose art_paths differ from the settled
                // ones, and the gate would fire a repaint forever.
                crate::search_cache_qt::put(&q_art, data);
                let data = data.clone();
                *LAST_CORT.lock().unwrap_or_else(|e| e.into_inner()) = Some(data.clone());
                crate::search_bridge::ui(move |mut b| {
                    let json = serde_json::to_string(&data).unwrap_or_else(|_| "{}".into());
                    b.as_mut().set_cortinilla_json(QString::from(json.as_str()));
                });
            }
        });
    }
}

/// Dismiss (Esc / click-outside / row activation / page change — the idle
/// timer is gone: the panel is focus-driven since the QoL round).
pub fn dismiss() {
    set_cortinilla_open(false);
    // Dismiss is a CANCELLATION POINT. Without the bump an in-flight load
    // survives the Esc and still publishes into a closed dropdown — and it
    // also rewrites LAST_CORT, which used to be what Enter submitted. The
    // bump makes every task in flight fail its version guard and exit.
    next_cort_version();
}

/// Arrow-key move (delta -1 up / +1 down; the Slint on_cortinilla_move_selection
/// semantics: Down from -1 -> first, Up from first -> -1, clamp both ends).
pub fn move_selection(delta: i32) {
    let current = CURRENT_SEL.load(Ordering::SeqCst);
    let snap = LAST_CORT.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(data) = snap else { return };
    let mut order: Vec<i32> = Vec::new();
    if let Some(top) = &data.top {
        order.push(top.flat_index);
    }
    for section in &data.sections {
        for row in &section.rows {
            order.push(row.flat_index);
        }
    }
    if order.is_empty() {
        return;
    }
    let pos = order.iter().position(|&fi| fi == current);
    let new_index = if delta > 0 {
        match pos {
            None => order[0],
            Some(p) if p + 1 < order.len() => order[p + 1],
            Some(_) => order[order.len() - 1],
        }
    } else {
        match pos {
            None => -1,
            Some(0) => -1,
            Some(p) => order[p - 1],
        }
    };
    // Content-space top-y of the selected row (the QML scrolls it into
    // view). Layout mirrors the QML cortinilla: 6px top padding; the
    // top-result block is 4 + 22 + 68; each section is 4 + 24 header +
    // 68/row.
    let scroll_y = flat_index_content_y(&data, new_index);
    set_selected(new_index, scroll_y);
}

fn flat_index_content_y(data: &CortinillaData, flat_index: i32) -> f64 {
    // Base 0.0, NOT 6.0. The panel's 6px top padding is a Flickable VIEWPORT
    // margin (`anchors.topMargin`), which shrinks the visible window rather
    // than shifting the content, so content-space y starts at zero. With a
    // 6.0 base every row was reported 6px low: the scroll-into-view handler
    // over-scrolled by 6px going down and clipped the row's top going up.
    // The immersive twin, written later against the same layout, already
    // starts at 0.0 — this brings the desktop arm in line.
    let mut y = 0.0f64;
    if let Some(top) = &data.top {
        if top.flat_index == flat_index {
            return y + 4.0 + 22.0;
        }
        y += 4.0 + 22.0 + 68.0;
    }
    for section in &data.sections {
        y += 4.0 + 24.0;
        for row in &section.rows {
            if row.flat_index == flat_index {
                return y;
            }
            y += 68.0;
        }
    }
    0.0
}

/// Click / Enter on a row: record the interaction (Capa B) and dispatch by
/// kind. Qobuz rows go to the catalog views; LOCAL rows go to the local seams
/// (album by group key, artist BY NAME, track from the snapshot). The QML
/// clears the input and closes itself.
///
/// Local rows are deliberately NOT recorded into the learned ranking: their
/// ids are in a different space from the catalog's, and a local artist row
/// has no id at all, so a bucket keyed on them could never be matched again.
pub fn row_clicked(flat_index: i32) {
    // TRACED, because this router had THREE ways to do nothing without saying
    // so — no snapshot, no matching flat index, and an unhandled kind — and the
    // owner reported a local row whose click produced no effect AND no log
    // line. A silent no-op is indistinguishable from a dead signal from the
    // outside; whichever of these fires, the log now names it.
    let snap = LAST_CORT.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(data) = snap else {
        log::warn!("[qbz-qt] cortinilla click {flat_index}: NO SNAPSHOT (LAST_CORT is None)");
        return;
    };
    let row = data
        .top
        .iter()
        .chain(data.sections.iter().flat_map(|s| s.rows.iter()))
        .find(|r| r.flat_index == flat_index)
        .cloned();
    let Some(row) = row else {
        let have: Vec<i32> = data
            .top
            .iter()
            .chain(data.sections.iter().flat_map(|s| s.rows.iter()))
            .map(|r| r.flat_index)
            .collect();
        log::warn!(
            "[qbz-qt] cortinilla click {flat_index}: NO ROW with that flat index — \
             snapshot holds {have:?}"
        );
        return;
    };
    log::info!(
        "[qbz-qt] cortinilla click {flat_index}: kind={} source={} id={}",
        row.kind,
        row.source,
        row.id
    );
    set_cortinilla_open(false);
    if row.source != "local" {
        let action = if row.kind == "track" {
            InteractionAction::Play
        } else {
            InteractionAction::Open
        };
        record(&attribution_query(&data.query), &row.kind, &row.id, action);
    }
    // LOCAL rows route through the local seams, never the Qobuz ones: their
    // ids live in a different space entirely (an album id is a group key, an
    // artist row has NO id, a track id is a library row id). Handing any of
    // them to the catalog routes would 404 or, worse, open someone else's
    // album that happens to share the number.
    if row.source == "local" {
        match row.kind.as_str() {
            // Through the SHARED album router, not straight to the loader.
            // `open_album_by_id` only publishes the document — it does not
            // navigate — so calling it alone loaded the album into a view that
            // was never shown: the click did nothing and logged nothing
            // (owner report, 2026-08-13, on `plex:4868...`). `crate::open_album`
            // is the one place that pairs the load with `nav_qt::record` +
            // `set_current_view`, and its `is_local_feed_id` arm already
            // recognises both `plex:` keys and path-shaped local group keys.
            "album" => crate::open_album(row.id),
            // BY NAME — local artists have no id (search_local.rs). Same shape
            // as the album row: `open_artist_by_name` only parks the name in
            // `local_pending_artist` for the Artists tab to consume, so the
            // view has to be brought up for anything to consume it.
            "artist" => {
                crate::nav_qt::record("local");
                crate::shell_bridge::ui(|mut b| {
                    b.as_mut().set_current_view(QString::from("local"))
                });
                crate::local_album_actions::open_artist_by_name(row.title);
            }
            "track" => play_local_row(&row.id),
            other => log::warn!(
                "[qbz-qt] cortinilla click: local row kind {other:?} has no route — nothing done"
            ),
        }
        return;
    }
    match row.kind.as_str() {
        "album" => crate::open_album(row.id),
        "artist" => crate::open_artist(row.id),
        "track" => {
            if let Ok(id) = row.id.parse::<u64>() {
                crate::play_track(id);
            }
        }
        "playlist" => crate::open_playlist(row.id),
        _ => {}
    }
}

/// One ⋯-menu action against the exact cortinilla snapshot on screen.
/// Navigation delegates to [`row_clicked`] so its attribution and local
/// routing stay single-owned; playback/queue actions mirror the canonical
/// card menus without making QML guess whether an id belongs to Qobuz or the
/// Local Library.
pub fn row_menu_action(flat_index: i32, action: &str) {
    if action == "open" {
        row_clicked(flat_index);
        return;
    }
    if !matches!(
        action,
        "play" | "next" | "later" | "queue" | "add-to-playlist" | "open-album"
    ) {
        log::warn!("[qbz-qt] cortinilla menu: unknown action {action:?}");
        return;
    }

    let snap = LAST_CORT.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(data) = snap else {
        log::warn!("[qbz-qt] cortinilla menu {flat_index}/{action}: no snapshot");
        return;
    };
    let row = data
        .top
        .iter()
        .chain(data.sections.iter().flat_map(|section| section.rows.iter()))
        .find(|row| row.flat_index == flat_index)
        .cloned();
    let Some(row) = row else {
        log::warn!("[qbz-qt] cortinilla menu {flat_index}/{action}: stale row");
        return;
    };

    log::info!(
        "[qbz-qt] cortinilla menu {action}: kind={} source={} id={}",
        row.kind,
        row.source,
        row.id,
    );
    if action == "open-album" {
        set_cortinilla_open(false);
        // `open_album` routes BOTH id spaces (its is_local_feed_id arm
        // recognises local group keys), so no source split here. The QML
        // entry is gated on a non-empty albumId; an empty one is a stale
        // snapshot, not a user path.
        if row.kind != "track" || row.album_id.is_empty() {
            log::warn!(
                "[qbz-qt] cortinilla menu open-album: kind={} albumId empty={}",
                row.kind,
                row.album_id.is_empty()
            );
            return;
        }
        crate::open_album(row.album_id);
        return;
    }
    if action == "add-to-playlist" {
        set_cortinilla_open(false);
        if row.kind != "track" {
            log::warn!(
                "[qbz-qt] cortinilla menu add-to-playlist: {} is not a track",
                row.kind
            );
        } else if row.source == "local" {
            open_local_cort_row_in_playlist_picker(&row.id);
        } else {
            crate::playlist_picker_qt::open_for_ids(&crate::app(), vec![row.id]);
        }
        return;
    }
    if action == "play" {
        set_cortinilla_open(false);
        if row.source != "local" {
            record(
                &attribution_query(&data.query),
                &row.kind,
                &row.id,
                InteractionAction::Play,
            );
        }
        if row.source == "local" {
            match row.kind.as_str() {
                "album" => {
                    let runtime = crate::app();
                    crate::spawn(async move {
                        crate::local_playback::play_album(&runtime, row.id, None, false).await;
                    });
                }
                "track" => play_local_row(&row.id),
                other => {
                    log::warn!("[qbz-qt] cortinilla menu play: local {other:?} is not playable")
                }
            }
            return;
        }
        match row.kind.as_str() {
            "album" => crate::play_album(row.id),
            "artist" => crate::play_artist_card(row.id),
            "track" => match row.id.parse::<u64>() {
                Ok(id) => crate::play_track(id),
                Err(_) => log::warn!("[qbz-qt] cortinilla menu play: invalid track id"),
            },
            "playlist" => match row.id.parse::<u64>() {
                Ok(id) => crate::play_playlist_by_id(id),
                Err(_) => log::warn!("[qbz-qt] cortinilla menu play: invalid playlist id"),
            },
            other => log::warn!("[qbz-qt] cortinilla menu play: unknown kind {other:?}"),
        }
        return;
    }

    let mode = action.to_string();
    if row.source == "local" {
        match row.kind.as_str() {
            "album" => {
                let runtime = crate::app();
                crate::spawn(async move {
                    crate::local_playback::enqueue(&runtime, "album".into(), row.id, mode).await;
                });
            }
            "track" => enqueue_local_cort_row(&row.id, mode),
            other => {
                log::warn!("[qbz-qt] cortinilla menu enqueue: local {other:?} is not queueable")
            }
        }
        return;
    }
    match row.kind.as_str() {
        "album" => crate::enqueue_album(row.id, mode),
        "track" => match row.id.parse::<u64>() {
            Ok(id) => crate::enqueue_track(id, mode),
            Err(_) => log::warn!("[qbz-qt] cortinilla menu enqueue: invalid track id"),
        },
        "playlist" => match row.id.parse::<u64>() {
            Ok(id) => crate::enqueue_playlist_by_id(id, mode),
            Err(_) => log::warn!("[qbz-qt] cortinilla menu enqueue: invalid playlist id"),
        },
        other => log::warn!("[qbz-qt] cortinilla menu enqueue: {other:?} is not queueable"),
    }
}

/// Record an interaction made on the SEARCH RESULTS PAGE.
///
/// The port had exactly ONE record site — the cortinilla row click — so
/// anything the user did on the results page taught the ranking nothing. That
/// is a bigger hole than it sounds, because it breaks the case the whole
/// feature exists for:
///
///   Search "one metallica" -> the right track is first -> click. Learned.
///   Search "one" -> Qobuz buries it among hundreds of "One"s, so it is not
///   in the dropdown -> "View more" -> click it on the page -> NOTHING is
///   learned, so the next "one" is just as bad.
///
/// With this, that page click teaches the bucket "one", and the next "one"
/// gets the track pulled to the front by `rank_within` before truncation, or
/// promoted outright as the top result by `top_for_query`.
///
/// Gated exactly like the reference (`record_search_interaction`): only while
/// the SEARCH view is current, only while the module is enabled, only with a
/// non-empty query. The query is the PAGE's, which is the committed one by
/// definition — the user pressed Enter or followed a "View more" to get here.
///
/// LOCAL entities never reach this path: local rows do not appear on the
/// results page, and their ids are a different space.
pub(crate) fn record_page_interaction(kind: &str, id: &str, action: InteractionAction) {
    if crate::nav_qt::current_view() != "search" {
        return;
    }
    if !is_enabled() {
        return;
    }
    let query = PAGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|p| p.doc.query.clone())
        .unwrap_or_default();
    if query.trim().is_empty() {
        return;
    }
    record(&query, kind, id, action);
}

/// Which query a click should be LEARNED against.
///
/// The payload's query is what produced the rows on screen, but it can lag
/// what the user has typed: they keep typing while the previous payload is
/// still shown, then click. Attributing to the payload teaches a bucket named
/// after a half-typed word — `annen` learning from a click the user made while
/// typing `annenmaykantereit`. On the owner's real store, 25 of 143 buckets
/// are typing prefixes like that, and none of them will ever be typed again as
/// a whole query, so the score is simply lost.
///
/// The rule is deliberately narrow: promote to the live query ONLY when the
/// payload's query is a STRICT PREFIX of it. Anything else — the user cleared
/// the box, typed something unrelated, or narrowed rather than extended — is
/// attributed as before. A broader rule would start moving interactions
/// between unrelated buckets, which is worse than the problem.
fn attribution_query(payload_query: &str) -> String {
    let live = crate::search_bridge::cortinilla_query();
    let live_key = live.trim();
    if !live_key.is_empty()
        && live_key.len() > payload_query.len()
        && live_key
            .to_lowercase()
            .starts_with(&payload_query.to_lowercase())
    {
        return live_key.to_string();
    }
    payload_query.to_string()
}

/// Play a local cortinilla track row from the per-query snapshot.
///
/// The row id is NOT re-resolved against the database: the snapshot is the
/// exact vector the payload was built from, so playing from it guarantees the
/// queue matches what the user is looking at — including the Plex rows that
/// were prepended, which a fresh lookup by id would not reproduce in order.
///
/// The queue is the WHOLE fetched vector (76 rows normally, 136 expanded)
/// starting at the clicked row, which is the reference's behaviour: the
/// dropdown shows three of them, but activating one gives you a real queue
/// rather than a single orphan track.
fn play_local_row(row_id: &str) {
    let rows = LAST_CORT_LOCAL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    // STRICT position: if the clicked row is not in the snapshot, do nothing.
    // Falling back to 0 would silently play a DIFFERENT track than the one
    // under the cursor, which is worse than not responding.
    let Some(start) = rows.iter().position(|t| t.id.to_string() == row_id) else {
        log::warn!("[qbz-qt] cortinilla: local row {row_id} not in the snapshot — ignored");
        return;
    };
    let runtime = crate::app();
    crate::spawn(async move {
        crate::local_playback::play_rows(&runtime, rows, start, false).await;
    });
}

/// Queue one local result from the same raw snapshot that produced the row.
/// In particular this preserves Plex/media-server rows that cannot be looked
/// back up in `library.db` by their synthetic id.
fn enqueue_local_cort_row(row_id: &str, mode: String) {
    let track = LAST_CORT_LOCAL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|track| track.id.to_string() == row_id)
        .cloned();
    let Some(track) = track else {
        log::warn!("[qbz-qt] cortinilla: local row {row_id} not in snapshot — enqueue ignored");
        return;
    };
    let runtime = crate::app();
    crate::spawn(async move {
        crate::local_playback::enqueue_rows(&runtime, vec![track], mode).await;
    });
}

/// Open the playlist picker for the exact local row rendered by search.
/// `QbzPlaylistPicker.openForLocalRow` serves a different identity space: an
/// already-open local-playlist detail. Search owns a `LocalTrack` snapshot, so
/// it must derive the source-aware local/Plex ref from that track instead.
fn open_local_cort_row_in_playlist_picker(row_id: &str) {
    let track = LAST_CORT_LOCAL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|track| track.id.to_string() == row_id)
        .cloned();
    let Some(track) = track else {
        log::warn!(
            "[qbz-qt] cortinilla: local row {row_id} not in snapshot — playlist picker ignored"
        );
        return;
    };
    crate::local_album_actions::open_picker_for_rows(std::slice::from_ref(&track));
}

/// "View more" on a section: full search page on the matching tab
/// (album -> 1, track -> 2, artist -> 3, playlist -> 4).
pub async fn view_more(runtime: &Arc<AppRuntime<LoggingAdapter>>, kind: &str) {
    // The three LOCAL sections leave the search surface entirely: their
    // "View more" opens the matching Local Library tab, because the Qobuz
    // results page has no on-device content to show and landing there would
    // read as the link having done nothing.
    let local_tab = match kind {
        "local-album" => Some("albums"),
        "local-artist" => Some("artists"),
        "local" => Some("tracks"),
        _ => None,
    };
    if let Some(tab) = local_tab {
        let q = crate::search_bridge::cortinilla_query();
        set_cortinilla_open(false);
        // The query pre-filters the TRACKS tab only — albums and artists have
        // no search box of their own, so passing it there would be a lie.
        crate::local_album_actions::set_pending_route(tab, if tab == "tracks" { &q } else { "" });
        crate::navigate_to("local");
        return;
    }
    let tab = match kind {
        "album" => 1,
        "track" => 2,
        "artist" => 3,
        "playlist" => 4,
        _ => 0,
    };
    // The LIVE query, not the last successfully loaded payload's: a fetch can
    // still be in flight (and `dismiss` is not a cancellation point), so
    // LAST_CORT lags whatever the user has actually typed.
    let q = crate::search_bridge::cortinilla_query();
    set_cortinilla_open(false);
    submit(runtime, &q, Some(tab)).await;
}

/// The Enter affordance with no keyboard selection: full search, All tab.
pub async fn search_all_action(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    // See `view_more`: the live query, never LAST_CORT's.
    let q = crate::search_bridge::cortinilla_query();
    set_cortinilla_open(false);
    submit(runtime, &q, Some(0)).await;
}

// ---------------------------------------------------------------------------
// Immersive search controller (contract §3.4 — port of main.rs:10359-10752)
// ---------------------------------------------------------------------------
//
// The Slint immersive search is a SEPARATE Rust controller from the desktop
// cortinilla: it publishes on `QbzImmersive.immSearch*` (NEVER on
// `QbzBridge.cortinilla*` — the desktop Cortinilla self-gates on
// `cortinillaOpen`, which must stay false while immersive is open), it loads
// Artists/Albums/Playlists ONLY (no tracks, NO top-result hero), and a row
// activation ACTS ON PLAYBACK per the `immersive_search_action` pref instead
// of navigating — immersive has no navigation (main.rs:10362-10365), so
// `search_qt::row_clicked`'s `crate::open_album/open_artist` dispatch is the
// exact behavior Slint rejects and is NOT reused here.
//
// The 220 ms debounce + the >=2-char gate + the version guard live HERE in
// Rust (Slint-faithful, trap 17 — the desktop cortinilla's QML debounce in
// HeaderBar.qml is NOT reused); the QML field calls `QbzImmersive.searchLive`
// per keystroke.

/// Per-category caps for the IMMERSIVE cortinilla (search.rs:609-611).
const IMMERSIVE_CAP_ARTISTS: usize = 2;
const IMMERSIVE_CAP_ALBUMS: usize = 5;
const IMMERSIVE_CAP_PLAYLISTS: usize = 2;

/// Immersive controller state — an OWN version counter (1:1
/// `next_immersive_search_version`, main.rs:10415) and an own snapshot, fully
/// disjoint from the desktop cortinilla's CORT_VERSION / LAST_CORT.
static LAST_IMM: Mutex<Option<CortinillaData>> = Mutex::new(None);
static IMM_VERSION: AtomicU64 = AtomicU64::new(0);
/// The keyboard-selected flat index mirrored from the bridge property (so
/// `imm_move_selection` needs no QML round-trip — the desktop CURRENT_SEL
/// pattern, :682).
static IMM_SEL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// Rust-side mirror of `QbzImmersive.immSearchOpen` — same pattern as
/// CORT_OPEN above: every write goes through one of the four
/// `set_imm_search_open` sites below and QML never writes the property
/// directly. Read by the hotkeys pipeline's (B) dropdown-steal
/// (2026-08-03 hotkeys-port contract §1.1(B): `QbzImmersive.open &&
/// QbzImmersive.immSearchOpen` wins over the desktop cortinilla).
static IMM_SEARCH_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn imm_search_open() -> bool {
    IMM_SEARCH_OPEN.load(Ordering::SeqCst)
}

fn next_imm_version() -> u64 {
    IMM_VERSION.fetch_add(1, Ordering::SeqCst) + 1
}
fn is_current_imm_version(v: u64) -> bool {
    IMM_VERSION.load(Ordering::SeqCst) == v
}

/// search.rs `map_search_all_to_immersive` (:617-706): sections **Artists,
/// Albums, Playlists IN THAT ORDER**, caps 2/5/2, **NO track rows, NO top
/// result** (immersive has no navigation — selecting acts on the queue). The
/// desktop mapper (`map_search_all_to_cortinilla`) builds a top result +
/// track rows + wrong caps and is NOT reused. Intra-category order still
/// applies the learned ranking before truncation (search.rs `take`).
fn map_search_all_to_immersive(query: &str, results: &SearchAllResults) -> CortinillaData {
    let to_artist_row = |a: &Artist| CortRow {
        kind: "artist".into(),
        id: a.id.to_string(),
        source: "qobuz".into(),
        title: a.name.clone(),
        subtitle: map_artist(a).subtitle,
        // Cortinilla dropdown row art (~40px): full variant (best()) — the
        // thumbnail down-tier was reverted after the 2026-08-15 owner smoke
        // (contract 04 §3).
        art_url: a
            .image
            .as_ref()
            .and_then(|i| i.best().cloned())
            .unwrap_or_default(),
        ..Default::default()
    };
    let to_album_row = |al: &Album| {
        let m = map_album(al);
        let (bit_depth, sample_rate) = crate::home_qt::album_audio_parts(al);
        let quality_detail = if bit_depth.is_some() && sample_rate.is_some() {
            crate::home_qt::quality_detail_from_parts(bit_depth, sample_rate)
        } else {
            String::new()
        };
        CortRow {
            kind: "album".into(),
            id: m.id,
            source: "qobuz".into(),
            title: m.title,
            subtitle: m.artist,
            quality_detail,
            art_url: m.art_url,
            ..Default::default()
        }
    };
    let to_playlist_row = |p: &Playlist| {
        let m = map_playlist(p);
        CortRow {
            kind: "playlist".into(),
            id: m.id,
            source: "qobuz".into(),
            title: m.title,
            subtitle: m.subtitle,
            art_url: m.art_url,
            ..Default::default()
        }
    };

    let mut artist_rows: Vec<CortRow> = results.artists.items.iter().map(&to_artist_row).collect();
    let mut album_rows: Vec<CortRow> = results.albums.items.iter().map(&to_album_row).collect();
    let mut playlist_rows: Vec<CortRow> = results
        .playlists
        .items
        .iter()
        .map(&to_playlist_row)
        .collect();
    // Intra-category order applies the learned ranking BEFORE truncation
    // (search.rs `take`); the caps themselves live in the assembly seam.
    rank_within(query, "artist", &mut artist_rows, |r| r.id.clone());
    rank_within(query, "album", &mut album_rows, |r| r.id.clone());
    rank_within(query, "playlist", &mut playlist_rows, |r| r.id.clone());

    assemble_immersive_sections(
        query,
        artist_rows,
        album_rows,
        playlist_rows,
        (
            results.artists.total,
            results.albums.total,
            results.playlists.total,
        ),
    )
}

/// Section assembly, factored out of `map_search_all_to_immersive` so the
/// caps/order/flat-index contract is unit-testable without a SearchAllResults
/// fixture: caps Artists 2 / Albums 5 / Playlists 2 (search.rs:609-611),
/// sections Artists, Albums, Playlists IN THAT ORDER (search.rs:690-692),
/// empty sections dropped, `top: None`, flat indices from 1
/// (`assign_flat_indices`'s no-top arm, :664). Callers pass RANKED,
/// UNCAPPED rows; the caps are applied here.
fn assemble_immersive_sections(
    query: &str,
    mut artist_rows: Vec<CortRow>,
    mut album_rows: Vec<CortRow>,
    mut playlist_rows: Vec<CortRow>,
    totals: (u32, u32, u32),
) -> CortinillaData {
    artist_rows.truncate(IMMERSIVE_CAP_ARTISTS);
    album_rows.truncate(IMMERSIVE_CAP_ALBUMS);
    playlist_rows.truncate(IMMERSIVE_CAP_PLAYLISTS);
    let mut sections: Vec<CortSection> = Vec::new();
    let mut push = |title: &str, kind: &str, rows: Vec<CortRow>, total: u32| {
        if !rows.is_empty() {
            sections.push(CortSection {
                title: title.to_string(),
                kind: kind.to_string(),
                has_more: total as usize > rows.len(),
                rows,
            });
        }
    };
    push(&qbz_i18n::t("Artists"), "artist", artist_rows, totals.0);
    push(&qbz_i18n::t("Albums"), "album", album_rows, totals.1);
    push(
        &qbz_i18n::t("Playlists"),
        "playlist",
        playlist_rows,
        totals.2,
    );

    let mut data = CortinillaData {
        query: query.to_string(),
        top: None,
        sections,
    };
    assign_flat_indices(&mut data);
    data
}

/// `QbzImmersive.searchLive` (the immersive `on_live`, main.rs:10376-10472):
/// gate -> 2-char gate -> open + reset -> debounced version-guarded load.
/// `overlay_open` is read by the bridge invokable (the property lives on the
/// Qt thread); everything else is re-read FRESH here.
pub fn imm_live(overlay_open: bool, query: String) {
    // Gate: only while the immersive overlay is open AND the configured
    // action is not "disabled" (the action doubles as the enable switch,
    // main.rs:10380-10386). The pref is re-read on EVERY keystroke — it can
    // change in Settings while immersive is open (main.rs:10384). disabled ⇒
    // no-op: imm_search_open NEVER flips true.
    if !overlay_open {
        return;
    }
    if crate::settings_qt::pref_str("immersive_search_action", "replace") == "disabled" {
        return;
    }
    let q = query.trim().to_string();
    // chars().count(): grapheme-ish length so a 2-char multibyte query (CJK)
    // is not rejected (main.rs:10390).
    if q.chars().count() < 2 {
        // Below the threshold — cancel the pending debounce (the version bump
        // makes the sleeping task stale, main.rs:10392's timer stop) and close
        // the dropdown so a backspaced query leaves no stale one open.
        next_imm_version();
        IMM_SEL.store(-1, Ordering::SeqCst);
        IMM_SEARCH_OPEN.store(false, Ordering::SeqCst);
        crate::immersive_bridge::ui(|mut b| {
            b.as_mut().set_imm_search_open(false);
        });
        return;
    }

    // Open + loading; ALWAYS reset selection + scroll on every refine —
    // never leave a stale "active row" from a prior query
    // (main.rs:10403-10407). Arrow nav fires no keystroke through here, so it
    // is unaffected.
    IMM_SEL.store(-1, Ordering::SeqCst);
    IMM_SEARCH_OPEN.store(true, Ordering::SeqCst);
    crate::immersive_bridge::ui(|mut b| {
        b.as_mut().set_imm_search_open(true);
        b.as_mut().set_imm_search_loading(true);
        b.as_mut().set_imm_search_selected_index(-1);
        b.as_mut().set_imm_search_scroll_y(0.0);
    });
    // Offline OR an unauthenticated session → widen the on-device album cap
    // (main.rs:10409-10414 reads OfflineState.offline || offline_session).
    let status = crate::offline_fwd::engine().status();
    let expand_local = status.is_offline() || status.offline_session;
    let version = next_imm_version();
    let runtime = crate::app();
    crate::spawn(async move {
        // 220 ms RUST debounce (main.rs:10422-10471): a newer keystroke bumps
        // the version, and this task exits without loading when it wakes.
        tokio::time::sleep(std::time::Duration::from_millis(220)).await;
        if !is_current_imm_version(version) {
            return;
        }
        imm_load(&runtime, q, expand_local, version).await;
    });
}

/// The debounced load (search.rs `load_immersive_search`, :1153-1199): Qobuz
/// catalog + local albums CONCURRENTLY; a Qobuz failure degrades to a
/// local-only dropdown instead of discarding everything.
async fn imm_load(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    q: String,
    expand_local: bool,
    version: u64,
) {
    // Blacklist filtering happens INSIDE search_all (`search.rs:1112-1115`),
    // same as the desktop live path (:735).
    let (bl, abl) = blacklist_snapshots();
    let caps = crate::search_local::LocalCaps::for_session(expand_local);
    let q_local = q.clone();
    let limit = caps.fetch_limit();
    let (results, local_rows) = tokio::join!(
        runtime.core().search_all(&q, &bl, &abl),
        crate::search_local::load_cortinilla_local(q_local, limit, false),
    );
    let mut data = match results {
        Ok(r) => map_search_all_to_immersive(&q, &r),
        Err(e) => {
            // Qobuz failed (offline / API error). The on-device rows resolved
            // independently, so still build a dropdown from JUST the local
            // section (search.rs:1186-1193). An empty local set then yields an
            // empty payload (the overlay shows only "No results for …").
            log::error!("[qbz-qt] immersive search load failed: {e}");
            CortinillaData {
                query: q.clone(),
                top: None,
                sections: Vec::new(),
            }
        }
    };
    crate::search_local::append_immersive_local_albums(&mut data, &local_rows, caps.albums, &q);

    // Artwork: disk hits inline; misses (Qobuz CDN + Plex thumbs) download in
    // the background and republish the SAME payload, version-guarded
    // (:461-481,:755-801).
    let mut urls: Vec<(String, &mut String)> = Vec::new();
    for section in &mut data.sections {
        for row in &mut section.rows {
            urls.push((row.art_url.clone(), &mut row.art_path));
        }
    }
    let missing = attach_urls(urls);
    *LAST_IMM.lock().unwrap() = Some(data.clone());
    crate::immersive_bridge::ui(move |mut b| {
        if is_current_imm_version(version) {
            let json = serde_json::to_string(&data).unwrap_or_else(|_| "{}".into());
            b.as_mut().set_imm_search_json(QString::from(json.as_str()));
            b.as_mut().set_imm_search_loading(false);
        }
    });
    if !missing.is_empty() {
        crate::spawn(async move {
            crate::artwork_qt::download_missing(missing).await;
            if !is_current_imm_version(version) {
                return;
            }
            let mut snap = LAST_IMM.lock().unwrap().clone();
            if let Some(data) = &mut snap {
                let mut urls: Vec<(String, &mut String)> = Vec::new();
                for section in &mut data.sections {
                    for row in &mut section.rows {
                        urls.push((row.art_url.clone(), &mut row.art_path));
                    }
                }
                let _ = attach_urls(urls);
                let data = data.clone();
                *LAST_IMM.lock().unwrap() = Some(data.clone());
                crate::immersive_bridge::ui(move |mut b| {
                    let json = serde_json::to_string(&data).unwrap_or_else(|_| "{}".into());
                    b.as_mut().set_imm_search_json(QString::from(json.as_str()));
                });
            }
        });
    }
}

/// Arrow-key move (the immersive `on_move_selection`, main.rs:10506-10548).
/// The immersive payload has NO top result, so the navigable order is built
/// from section rows only (flat indices start at 1). Both ends clamp — NO
/// wrap.
pub fn imm_move_selection(delta: i32) {
    let current = IMM_SEL.load(Ordering::SeqCst);
    let snap = LAST_IMM.lock().unwrap().clone();
    let Some(data) = snap else { return };
    let order: Vec<i32> = data
        .sections
        .iter()
        .flat_map(|s| s.rows.iter().map(|r| r.flat_index))
        .collect();
    if order.is_empty() {
        return;
    }
    let new_index = imm_next_selection(&order, current, delta);
    // Content-top y of the selected row so the overlay scrolls it into view
    // (main.rs:10520-10548). The immersive cortinilla has NO top-result
    // block: each section block = padTop 4 + header 24, rows are 56. These
    // constants MUST match ImmersiveSearchCortinilla.qml's layout.
    let scroll_y = imm_scroll_y(&data, new_index);
    IMM_SEL.store(new_index, Ordering::SeqCst);
    crate::immersive_bridge::ui(move |mut b| {
        b.as_mut().set_imm_search_selected_index(new_index);
        b.as_mut().set_imm_search_scroll_y(scroll_y);
    });
}

/// The clamp, verbatim (main.rs:10524-10540): Down from -1 lands on the
/// first row, Up from the first row returns to -1, both ends clamp.
fn imm_next_selection(order: &[i32], current: i32, delta: i32) -> i32 {
    let pos = order.iter().position(|&fi| fi == current);
    if delta > 0 {
        match pos {
            None => order[0],
            Some(p) if p + 1 < order.len() => order[p + 1],
            Some(_) => order[order.len() - 1],
        }
    } else {
        match pos {
            None => -1,
            Some(0) => -1,
            Some(p) => order[p - 1],
        }
    }
}

/// Content-space top-y of a flat index under the no-top-result layout:
/// per-section 28 (padTop 4 + header 24) + 56 per row (main.rs:10530-10547).
/// -1 (and any unknown index) scrolls to the top.
fn imm_scroll_y(data: &CortinillaData, flat_index: i32) -> f64 {
    if flat_index < 0 {
        return 0.0;
    }
    let mut y = 0.0f64;
    for section in &data.sections {
        y += 28.0; // padTop 4 + header 24
        for row in &section.rows {
            if row.flat_index == flat_index {
                return y;
            }
            y += 56.0; // row height
        }
    }
    0.0
}

/// `QbzImmersive.dismissSearch` (main.rs:10552-10564): clears the dropdown +
/// the selection.
pub fn imm_dismiss() {
    IMM_SEL.store(-1, Ordering::SeqCst);
    IMM_SEARCH_OPEN.store(false, Ordering::SeqCst);
    crate::immersive_bridge::ui(|mut b| {
        b.as_mut().set_imm_search_open(false);
        b.as_mut().set_imm_search_selected_index(-1);
    });
}

/// The round-2-verified dispatch decision (contract §3.4), factored pure so
/// the whole table is unit-testable. `action` is the FRESH pref value
/// ("replace" | "next" | "queue" — "disabled" is guarded by the caller).
#[derive(Debug, Clone, PartialEq)]
enum ImmDispatch {
    /// local_playback::play_album(rt, key, None, false)
    LocalPlay,
    /// local_playback::enqueue(rt, "album", key, mode) — it re-fetches +
    /// cover-fills itself; do NOT hand-roll the fetch_album_tracks_blocking +
    /// fill_missing_covers pair (contract §3.4).
    LocalEnqueue(String),
    /// playback_qt::play_album
    PlayAlbum,
    /// playlist_qt::play_playlist_by_id
    PlayPlaylist,
    /// playback_qt::play_artist
    PlayArtist,
    /// playback_qt::enqueue_album(rt, id, mode)
    EnqueueAlbum(String),
    /// playlist_qt::enqueue_playlist_by_id(rt, id, mode)
    EnqueuePlaylist(String),
    /// playback_qt::enqueue_artist_top_by_id(rt, id, mode)
    EnqueueArtistTop(String),
    None,
}

/// (row.source, row.kind, action) -> the playback target. Local album rows
/// branch BEFORE the Qobuz match — a local album's id is a group key, not a
/// numeric Qobuz id (main.rs:10614-10621).
fn imm_dispatch(source: &str, kind: &str, action: &str) -> ImmDispatch {
    if source == "local" {
        return match action {
            "replace" => ImmDispatch::LocalPlay,
            "next" | "queue" => ImmDispatch::LocalEnqueue(action.to_string()),
            _ => ImmDispatch::None,
        };
    }
    match (kind, action) {
        ("album", "replace") => ImmDispatch::PlayAlbum,
        ("playlist", "replace") => ImmDispatch::PlayPlaylist,
        ("artist", "replace") => ImmDispatch::PlayArtist,
        ("album", mode @ ("next" | "queue")) => ImmDispatch::EnqueueAlbum(mode.to_string()),
        ("playlist", mode @ ("next" | "queue")) => ImmDispatch::EnqueuePlaylist(mode.to_string()),
        ("artist", mode @ ("next" | "queue")) => ImmDispatch::EnqueueArtistTop(mode.to_string()),
        _ => ImmDispatch::None,
    }
}

/// `QbzImmersive.searchRowActivated` (main.rs:10579-10751): resolve the flat
/// index against the controller snapshot, close the dropdown + clear the
/// field (the user STAYS in immersive — no navigation), then dispatch to
/// playback per the FRESH pref.
pub fn imm_row_activated(flat_index: i32) {
    let snap = LAST_IMM.lock().unwrap().clone();
    let Some(data) = snap else { return };
    let row = data
        .sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .find(|r| r.flat_index == flat_index)
        .cloned();
    let Some(row) = row else { return };

    // Close the dropdown (STAY in immersive) AND clear the field, mirroring
    // the main cortinilla: once a result is activated, a lingering query
    // would re-invoke the dropdown when focus returns to the field
    // (main.rs:10595-10599).
    IMM_SEL.store(-1, Ordering::SeqCst);
    IMM_SEARCH_OPEN.store(false, Ordering::SeqCst);
    crate::immersive_bridge::ui(|mut b| {
        b.as_mut().set_imm_search_open(false);
        b.as_mut().set_imm_search_selected_index(-1);
        b.as_mut().set_search_input_text(QString::from(""));
    });

    // Read the configured action FRESH (it can change in Settings while
    // immersive is open — main.rs:10601-10608). "disabled" is also gated
    // upstream in imm_live, but guard here too in case the dropdown was
    // already open when it flipped.
    let action = crate::settings_qt::pref_str("immersive_search_action", "replace");
    if action == "disabled" {
        return;
    }

    let runtime = crate::app();
    match imm_dispatch(&row.source, &row.kind, &action) {
        ImmDispatch::LocalPlay => {
            let key = row.id.clone();
            crate::spawn(async move {
                crate::local_playback::play_album(&runtime, key, None, false).await;
            });
        }
        ImmDispatch::LocalEnqueue(mode) => {
            let key = row.id.clone();
            crate::spawn(async move {
                crate::local_playback::enqueue(&runtime, "album".to_string(), key, mode).await;
            });
        }
        ImmDispatch::PlayAlbum => {
            let id = row.id.clone();
            crate::spawn(async move {
                if let Err(e) = crate::playback_qt::play_album(&runtime, &id).await {
                    log::error!("[qbz-qt] immersive search play_album {id}: {e}");
                }
            });
        }
        ImmDispatch::PlayPlaylist => {
            if let Ok(pid) = row.id.parse::<u64>() {
                crate::spawn(async move {
                    if let Err(e) = crate::playlist_qt::play_playlist_by_id(&runtime, pid).await {
                        log::error!("[qbz-qt] immersive search play_playlist {pid}: {e}");
                    }
                });
            }
        }
        ImmDispatch::PlayArtist => {
            let id = row.id.clone();
            crate::spawn(async move {
                if let Err(e) = crate::playback_qt::play_artist(&runtime, &id).await {
                    log::error!("[qbz-qt] immersive search play_artist {id}: {e}");
                }
            });
        }
        ImmDispatch::EnqueueAlbum(mode) => {
            let id = row.id.clone();
            crate::spawn(async move {
                if let Err(e) = crate::playback_qt::enqueue_album(&runtime, &id, &mode).await {
                    log::error!("[qbz-qt] immersive search enqueue_album {id} ({mode}): {e}");
                }
            });
        }
        ImmDispatch::EnqueuePlaylist(mode) => {
            if let Ok(pid) = row.id.parse::<u64>() {
                crate::spawn(async move {
                    if let Err(e) =
                        crate::playlist_qt::enqueue_playlist_by_id(&runtime, pid, &mode).await
                    {
                        log::error!(
                            "[qbz-qt] immersive search enqueue_playlist {pid} ({mode}): {e}"
                        );
                    }
                });
            }
        }
        ImmDispatch::EnqueueArtistTop(mode) => {
            let id = row.id.clone();
            crate::spawn(async move {
                if let Err(e) =
                    crate::playback_qt::enqueue_artist_top_by_id(&runtime, &id, &mode).await
                {
                    log::error!("[qbz-qt] immersive search enqueue_artist_top {id} ({mode}): {e}");
                }
            });
        }
        ImmDispatch::None => {}
    }
}

// ---------------------------------------------------------------------------
// Results page
// ---------------------------------------------------------------------------

#[derive(Clone, Default, Serialize)]
pub struct MostPopularDoc {
    pub kind: String,
    pub album: Option<CardRow>,
    pub artist: Option<ArtistRow>,
    pub track: Option<TrackRow>,
    #[serde(rename = "qualityLabel")]
    pub quality_label: String,
}

#[derive(Clone, Default, Serialize)]
pub struct SearchPageDoc {
    #[serde(rename = "localRevision")]
    pub local_revision: String,
    #[serde(rename = "localSections")]
    pub local_sections: Vec<CortSection>,
    pub query: String,
    pub tab: i32,
    pub loading: bool,
    #[serde(rename = "filterIndex")]
    pub filter_index: i32,
    pub albums: Vec<CardRow>,
    pub tracks: Vec<TrackRow>,
    pub artists: Vec<ArtistRow>,
    #[serde(rename = "artistsCarousel")]
    pub artists_carousel: Vec<ArtistRow>,
    pub playlists: Vec<PlaylistRow>,
    #[serde(rename = "albumsTotal")]
    pub albums_total: i32,
    #[serde(rename = "tracksTotal")]
    pub tracks_total: i32,
    #[serde(rename = "artistsTotal")]
    pub artists_total: i32,
    #[serde(rename = "playlistsTotal")]
    pub playlists_total: i32,
    #[serde(rename = "mostPopular")]
    pub most_popular: MostPopularDoc,
}

#[derive(Default)]
struct PageState {
    doc: SearchPageDoc,
    local: crate::search_local::PageSnapshot,
}

static PAGE: Mutex<Option<PageState>> = Mutex::new(None);
static PAGE_VERSION: AtomicU64 = AtomicU64::new(0);

fn next_page_version() -> u64 {
    PAGE_VERSION.fetch_add(1, Ordering::SeqCst) + 1
}
fn is_current_page_version(v: u64) -> bool {
    PAGE_VERSION.load(Ordering::SeqCst) == v
}

/// A pin/unpin just landed: patch the CACHED page rows carrying `(kind, id)`.
///
/// Third sibling of `home_qt::apply_pin_change` and
/// `recommendations_qt::apply_pin_change`, and it publishes nothing for the
/// same reason they do not: the cards on screen are corrected in place by
/// `QbzLibrary.pinChanged`, so no model is replaced and no delegate is torn
/// down.
///
/// Why search NEEDED it, specifically: `PAGE` is not a build-once cache — the
/// artwork pass in `submit` re-publishes the SAME document ~a second later
/// with only `art_path` mutated, and `tab_changed` / `load_more` /
/// `filter_changed` each publish the cache too. Every one of those swaps the
/// model out from under the card, so a pin made in the meantime was reverted
/// by the stale `isPinned` the cache still held — the optimistic flip
/// visibly undone by artwork landing.
///
/// The alternative (re-stamping `is_pinned` from the store inside
/// `publish_page`) was rejected: `sidebar_qt::is_pinned` is a sqlite query per
/// row, so that would put one query per album + artist + playlist row on EVERY
/// publish, including the two that a single search already triggers. Patching
/// the cache once per pin click is O(rows) in memory and matches the shape the
/// other two caches already use — the reference does the same thing with its
/// `set_*_row_pinned` model walks.
///
/// Tracks are not pinnable, so the track list is not walked. The cortinilla
/// snapshot (`LAST_CORT`) is not walked either: `CortRow` carries no pin flag
/// and the dropdown draws no badge.
pub(crate) fn apply_pin_change(kind: &str, id: &str, pinned: bool) {
    if !matches!(kind, "album" | "artist" | "playlist") {
        return;
    }
    let Ok(mut guard) = PAGE.lock() else {
        return;
    };
    let Some(page) = guard.as_mut() else {
        return;
    };
    let doc = &mut page.doc;
    match kind {
        "album" => {
            for row in doc.albums.iter_mut().filter(|r| r.id == id) {
                row.is_pinned = pinned;
            }
            if let Some(row) = doc.most_popular.album.as_mut().filter(|r| r.id == id) {
                row.is_pinned = pinned;
            }
        }
        "artist" => {
            // `artists_carousel` holds CLONES of `artists` rows (the All-tab
            // dedupe copies them), so both lists have to be walked or the
            // carousel keeps the stale badge.
            for row in doc.artists.iter_mut().filter(|r| r.id == id) {
                row.is_pinned = pinned;
            }
            for row in doc.artists_carousel.iter_mut().filter(|r| r.id == id) {
                row.is_pinned = pinned;
            }
            if let Some(row) = doc.most_popular.artist.as_mut().filter(|r| r.id == id) {
                row.is_pinned = pinned;
            }
        }
        _ => {
            for row in doc.playlists.iter_mut().filter(|r| r.id == id) {
                row.is_pinned = pinned;
            }
        }
    }
}

/// A favourite toggle just SETTLED: patch the cached page rows.
///
/// Same shape as [`apply_pin_change`], same reason it publishes nothing — and
/// search needs it MORE than the pin twin does, because its cached document is
/// re-published on a timer the user does not control (the artwork pass after
/// `submit`). Heart an album in the results grid, wait for a cover to land,
/// and the model swap put the hollow heart back over an album now IN the
/// library; the next click removed it.
///
/// Tracks are included here (unlike the pin twin) — track rows do draw a
/// heart. The cortinilla snapshot is not: `CortRow` carries no favourite flag.
pub(crate) fn apply_favorite_change(kind: &str, id: &str, favorite: bool) {
    if !matches!(kind, "album" | "track" | "artist" | "playlist") {
        return;
    }
    let Ok(mut guard) = PAGE.lock() else {
        return;
    };
    let Some(page) = guard.as_mut() else {
        return;
    };
    let doc = &mut page.doc;
    match kind {
        "album" => {
            for row in doc.albums.iter_mut().filter(|r| r.id == id) {
                row.is_favorite = favorite;
            }
            if let Some(row) = doc.most_popular.album.as_mut().filter(|r| r.id == id) {
                row.is_favorite = favorite;
            }
        }
        "track" => {
            for row in doc.tracks.iter_mut().filter(|r| r.id == id) {
                row.is_favorite = favorite;
            }
            if let Some(row) = doc.most_popular.track.as_mut().filter(|r| r.id == id) {
                row.is_favorite = favorite;
            }
        }
        "artist" => {
            // `following` is this row type's heart. Both lists again — the
            // carousel holds clones (see `apply_pin_change`).
            for row in doc.artists.iter_mut().filter(|r| r.id == id) {
                row.following = favorite;
            }
            for row in doc.artists_carousel.iter_mut().filter(|r| r.id == id) {
                row.following = favorite;
            }
            if let Some(row) = doc.most_popular.artist.as_mut().filter(|r| r.id == id) {
                row.following = favorite;
            }
        }
        _ => {
            for row in doc.playlists.iter_mut().filter(|r| r.id == id) {
                row.is_favorite = favorite;
            }
        }
    }
}

fn publish_page(doc: &SearchPageDoc) {
    let json = serde_json::to_string(doc).unwrap_or_else(|_| "{}".into());
    let revision = doc.local_revision.clone();
    crate::search_bridge::ui(move |mut b| {
        let current = PAGE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|p| p.doc.local_revision == revision);
        if current {
            b.as_mut().set_search_json(QString::from(json.as_str()));
        }
    });
}

/// search.rs `search_type_for_filter`.
fn search_type_for_filter(index: i32) -> Option<String> {
    match index {
        1 => Some("MainArtist".into()),
        2 => Some("Performer".into()),
        3 => Some("Composer".into()),
        4 => Some("Label".into()),
        5 => Some("ReleaseName".into()),
        _ => None,
    }
}

/// Enter (or View-more / search-all): record the nav entry and run the full
/// combined search. `tab` is always All on plain submit; View-more picks
/// the section's tab (the Slint lands on the tab, the QML clears the header
/// input itself).
pub async fn submit(runtime: &Arc<AppRuntime<LoggingAdapter>>, query: &str, tab: Option<i32>) {
    submit_page(runtime, query, tab, true).await;
}

/// Rehydrate the query of a Kiosk history entry without navigating again.
pub async fn restore_kiosk_page(runtime: &Arc<AppRuntime<LoggingAdapter>>, query: &str, tab: i32) {
    if crate::kiosk_profile_qt::active() && crate::nav_qt::current_view() == "search" {
        submit_page(runtime, query, Some(tab), false).await;
    }
}

async fn submit_page(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    query: &str,
    tab: Option<i32>,
    navigate: bool,
) {
    let q = query.trim().to_string();
    if q.chars().count() < 2 {
        return;
    }
    let version = next_page_version();
    if navigate {
        crate::navigate_to("search");
    }
    {
        let mut guard = PAGE.lock().unwrap();
        if !is_current_page_version(version) {
            return;
        }
        let page = guard.get_or_insert_with(PageState::default);
        *page = PageState::default();
        page.doc.query = q.clone();
        page.doc.tab = tab.unwrap_or(0);
        page.doc.loading = true;
        page.doc.local_revision = version.to_string();
        publish_page(&page.doc);
    }

    let t = std::time::Instant::now();
    // Blacklist filtering happens INSIDE search_all (`search.rs:1056-1059`).
    let (bl, abl) = blacklist_snapshots();
    let (results, local_rows) = tokio::join!(
        runtime.core().search_all(&q, &bl, &abl),
        crate::search_local::load_cortinilla_local(q.clone(), 136, false),
    );
    if !is_current_page_version(version) {
        return;
    }
    let local = crate::search_local::PageSnapshot::new(&q, local_rows);
    // Local search has no Qobuz-auth prerequisite. A catalog error must not
    // clear successful local results or their exact playback snapshot.
    let results = match results {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[qbz-qt] Qobuz search failed; keeping local results: {e}");
            fn empty<T>() -> qbz_models::SearchResultsPage<T> {
                qbz_models::SearchResultsPage {
                    items: vec![],
                    total: 0,
                    offset: 0,
                    limit: 0,
                }
            }
            SearchAllResults {
                albums: empty(),
                tracks: empty(),
                artists: empty(),
                playlists: empty(),
                most_popular: None,
            }
        }
    };
    log::info!(
        "[qbz-qt][perf] search page \"{q}\" -> results: {:?} ({}+{}+{}+{})",
        t.elapsed(),
        results.albums.items.len(),
        results.tracks.items.len(),
        results.artists.items.len(),
        results.playlists.items.len(),
    );

    let mut albums: Vec<CardRow> = results.albums.items.iter().map(map_album).collect();
    let mut tracks: Vec<TrackRow> = results.tracks.items.iter().map(map_track).collect();
    let mut artists: Vec<ArtistRow> = results.artists.items.iter().map(map_artist).collect();
    let mut playlists: Vec<PlaylistRow> =
        results.playlists.items.iter().map(map_playlist).collect();

    let (mp_kind, mut mp_album, mut mp_artist, mut mp_track, mp_quality) =
        match &results.most_popular {
            Some(MostPopularItem::Albums(a)) => (
                "album".to_string(),
                Some(map_album(a)),
                None,
                None,
                quality_label(a.maximum_bit_depth, a.maximum_sampling_rate),
            ),
            Some(MostPopularItem::Artists(a)) => (
                "artist".to_string(),
                None,
                Some(map_artist(a)),
                None,
                String::new(),
            ),
            Some(MostPopularItem::Tracks(t)) => (
                "track".to_string(),
                None,
                None,
                Some(map_track(t)),
                quality_label(t.maximum_bit_depth, t.maximum_sampling_rate),
            ),
            None => (String::new(), None, None, None, String::new()),
        };
    let mut missing: Vec<String> = Vec::new();
    missing.extend(attach_urls(
        albums
            .iter_mut()
            .map(|r| (r.art_url.clone(), &mut r.art_path))
            .collect(),
    ));
    missing.extend(attach_urls(
        tracks
            .iter_mut()
            .map(|r| (r.art_url.clone(), &mut r.art_path))
            .collect(),
    ));
    missing.extend(attach_urls(
        artists
            .iter_mut()
            .map(|r| (r.art_url.clone(), &mut r.art_path))
            .collect(),
    ));
    missing.extend(attach_urls(
        playlists
            .iter_mut()
            .map(|r| (r.art_url.clone(), &mut r.art_path))
            .collect(),
    ));
    if let Some(a) = &mut mp_album {
        missing.extend(attach_urls(vec![(a.art_url.clone(), &mut a.art_path)]));
    }
    if let Some(a) = &mut mp_artist {
        missing.extend(attach_urls(vec![(a.art_url.clone(), &mut a.art_path)]));
    }
    if let Some(t) = &mut mp_track {
        missing.extend(attach_urls(vec![(t.art_url.clone(), &mut t.art_path)]));
    }
    missing.dedup();
    // The All-tab carousel skips the top-result artist when it is also the
    // first list entry (apply_search dedupe) — derived AFTER artwork attach
    // so its rows carry the disk paths (they are clones of `artists` rows).
    let artists_carousel: Vec<ArtistRow> = match (&mp_kind, artists.first()) {
        (kind, Some(first))
            if kind == "artist" && mp_artist.as_ref().is_some_and(|m| m.id == first.id) =>
        {
            artists[1..].to_vec()
        }
        _ => artists.clone(),
    };

    let doc = {
        let mut guard = PAGE.lock().unwrap();
        if !is_current_page_version(version) {
            return;
        }
        let page = guard.get_or_insert_with(PageState::default);
        page.local = local;
        let doc = &mut page.doc;
        doc.local_sections = page.local.data.sections.clone();
        missing.extend(attach_urls(
            doc.local_sections
                .iter_mut()
                .flat_map(|s| &mut s.rows)
                .map(|r| (r.art_url.clone(), &mut r.art_path))
                .collect(),
        ));
        doc.loading = false;
        doc.albums = albums;
        doc.tracks = tracks;
        doc.artists_carousel = artists_carousel;
        doc.artists = artists;
        doc.playlists = playlists;
        doc.albums_total = results.albums.total as i32;
        doc.tracks_total = results.tracks.total as i32;
        doc.artists_total = results.artists.total as i32;
        doc.playlists_total = results.playlists.total as i32;
        doc.most_popular = MostPopularDoc {
            kind: mp_kind,
            album: mp_album,
            artist: mp_artist,
            track: mp_track,
            quality_label: mp_quality,
        };
        doc.clone()
    };
    publish_page(&doc);

    if !crate::kiosk_profile_qt::active() && !missing.is_empty() {
        crate::spawn(async move {
            crate::artwork_qt::download_missing(missing).await;
            if !is_current_page_version(version) {
                return;
            }
            let doc = {
                let mut guard = PAGE.lock().unwrap();
                if !is_current_page_version(version) {
                    return;
                }
                let doc = &mut guard.get_or_insert_with(PageState::default).doc;
                let mut missing2 = Vec::new();
                missing2.extend(attach_urls(
                    doc.albums
                        .iter_mut()
                        .map(|r| (r.art_url.clone(), &mut r.art_path))
                        .collect(),
                ));
                missing2.extend(attach_urls(
                    doc.tracks
                        .iter_mut()
                        .map(|r| (r.art_url.clone(), &mut r.art_path))
                        .collect(),
                ));
                missing2.extend(attach_urls(
                    doc.artists
                        .iter_mut()
                        .map(|r| (r.art_url.clone(), &mut r.art_path))
                        .collect(),
                ));
                missing2.extend(attach_urls(
                    doc.playlists
                        .iter_mut()
                        .map(|r| (r.art_url.clone(), &mut r.art_path))
                        .collect(),
                ));
                // The carousel shares rows with `artists`; artPath lives on
                // the ArtistRow structs, so refresh the carousel from the
                // updated list.
                doc.artists_carousel = doc
                    .artists_carousel
                    .iter()
                    .map(|c| {
                        doc.artists
                            .iter()
                            .find(|a| a.id == c.id)
                            .cloned()
                            .unwrap_or_else(|| c.clone())
                    })
                    .collect();
                if let Some(a) = &mut doc.most_popular.album {
                    let _ = attach_urls(vec![(a.art_url.clone(), &mut a.art_path)]);
                }
                if let Some(a) = &mut doc.most_popular.artist {
                    let _ = attach_urls(vec![(a.art_url.clone(), &mut a.art_path)]);
                }
                if let Some(t) = &mut doc.most_popular.track {
                    let _ = attach_urls(vec![(t.art_url.clone(), &mut t.art_path)]);
                }
                let _ = attach_urls(
                    doc.local_sections
                        .iter_mut()
                        .flat_map(|s| &mut s.rows)
                        .map(|r| (r.art_url.clone(), &mut r.art_path))
                        .collect(),
                );
                let _ = missing2;
                doc.clone()
            };
            publish_page(&doc);
        });
    }
}

/// Resolve only the local snapshot that produced the visible page.
pub fn local_page_action(revision: &str, index: i32, action: &str) {
    let (row, tracks) = {
        let guard = PAGE.lock().unwrap_or_else(|e| e.into_inner());
        let Some(page) = guard.as_ref().filter(|p| p.doc.local_revision == revision) else {
            return;
        };
        let Some(row) = page.local.row(index) else {
            return;
        };
        (row.clone(), page.local.tracks.clone())
    };
    if action == "open" {
        match row.kind.as_str() {
            "album" => crate::open_album(row.id),
            "artist" => {
                crate::navigate_to("local");
                crate::local_album_actions::open_artist_by_name(row.title);
            }
            _ => {}
        }
        return;
    }
    if action == "open-album" && row.kind == "track" && !row.album_id.is_empty() {
        crate::open_album(row.album_id);
        return;
    }
    let start = row.id.parse::<usize>().ok().filter(|i| *i < tracks.len());
    if action == "add-to-playlist" && row.kind == "track" {
        if let Some(start) = start {
            crate::local_album_actions::open_picker_for_rows(&tracks[start..start + 1]);
        }
        return;
    }
    if !matches!(action, "play" | "next" | "later" | "queue") {
        return;
    }
    let runtime = crate::app();
    let action = action.to_string();
    crate::spawn(async move {
        match row.kind.as_str() {
            "album" if action == "play" => {
                crate::local_playback::play_album(&runtime, row.id, None, false).await;
            }
            "album" => {
                crate::local_playback::enqueue(&runtime, "album".into(), row.id, action).await;
            }
            "track" => {
                if let Some(start) = start {
                    if action == "play" {
                        crate::local_playback::play_rows(&runtime, tracks, start, false).await;
                    } else {
                        crate::local_playback::enqueue_rows(
                            &runtime,
                            vec![tracks[start].clone()],
                            action,
                        )
                        .await;
                    }
                }
            }
            _ => {}
        }
    });
}

pub fn local_page_more(revision: &str, kind: &str) {
    let query = {
        let guard = PAGE.lock().unwrap_or_else(|e| e.into_inner());
        let Some(page) = guard.as_ref().filter(|p| p.doc.local_revision == revision) else {
            return;
        };
        page.doc.query.clone()
    };
    let tab = match kind {
        "local-album" => "albums",
        "local-artist" => "artists",
        "local" => "tracks",
        _ => return,
    };
    crate::local_album_actions::set_pending_route(tab, if tab == "tracks" { &query } else { "" });
    crate::navigate_to("local");
}

/// Tab strip: pure view state (search_all already loaded everything).
pub fn tab_changed(tab: i32) {
    let mut guard = PAGE.lock().unwrap();
    if let Some(page) = guard.as_mut() {
        page.doc.tab = tab;
        let doc = page.doc.clone();
        drop(guard);
        publish_page(&doc);
    }
}

/// search.rs PAGE_SIZE (matches the Tauri search page size).
const PAGE_SIZE: u32 = 20;

/// Finish the artwork half of a page-2+ search request. `attach_urls` resolves
/// warm cache hits before the rows publish; this downloads the remaining URLs
/// and performs one guarded refresh so every appended card eventually carries
/// its real local path. The QML grid can show individual window hits sooner,
/// but this document refresh retires its loading placeholders definitively.
async fn refresh_loaded_page_art(version: u64, mut missing: Vec<String>) {
    // Kiosk mounted cells request their physical-size bucket through the
    // existing window/cache resolver; hidden tabs must not hydrate artwork.
    if crate::kiosk_profile_qt::active() || missing.is_empty() {
        return;
    }
    missing.sort();
    missing.dedup();
    crate::artwork_qt::download_missing(missing).await;
    if !is_current_page_version(version) {
        return;
    }
    let doc = {
        let mut guard = PAGE.lock().unwrap();
        let Some(page) = guard.as_mut() else { return };
        if !is_current_page_version(version) {
            return;
        }
        let doc = &mut page.doc;
        let _ = attach_urls(
            doc.albums
                .iter_mut()
                .map(|row| (row.art_url.clone(), &mut row.art_path))
                .collect(),
        );
        let _ = attach_urls(
            doc.tracks
                .iter_mut()
                .map(|row| (row.art_url.clone(), &mut row.art_path))
                .collect(),
        );
        let _ = attach_urls(
            doc.artists
                .iter_mut()
                .map(|row| (row.art_url.clone(), &mut row.art_path))
                .collect(),
        );
        let _ = attach_urls(
            doc.playlists
                .iter_mut()
                .map(|row| (row.art_url.clone(), &mut row.art_path))
                .collect(),
        );
        doc.clone()
    };
    publish_page(&doc);
}

/// Load more rows for the active per-type tab (offset = rows already loaded).
pub async fn load_more(runtime: &Arc<AppRuntime<LoggingAdapter>>, tab: i32) {
    let (query, filter, offset) = {
        let guard = PAGE.lock().unwrap();
        let Some(page) = guard.as_ref() else { return };
        let offset = match tab {
            1 => page.doc.albums.len(),
            2 => page.doc.tracks.len(),
            3 => page.doc.artists.len(),
            4 => page.doc.playlists.len(),
            _ => return,
        };
        (page.doc.query.clone(), page.doc.filter_index, offset as u32)
    };
    let search_type = search_type_for_filter(filter);
    let version = next_page_version();
    // Page-2+ is NOT filtered by the API: `search_albums`/`search_tracks`/
    // `search_artists` take no snapshot, so the drop happens here, exactly as
    // the reference does it (`search.rs:1616-1666`). `offset` stays the VISIBLE
    // row count (`main.rs:9795-9800` reads `row_count()`), so a filtered page
    // shortens the batch rather than shifting the cursor — reference-identical.
    let (bl, abl) = blacklist_snapshots();

    match tab {
        1 => match runtime
            .core()
            .search_albums(&query, PAGE_SIZE, offset, search_type.as_deref())
            .await
        {
            Ok(page) => {
                let mut rows: Vec<CardRow> = page
                    .items
                    .iter()
                    .filter(|a| !qbz_core::core::album_blacklisted(a, &bl, &abl))
                    .map(map_album)
                    .collect();
                let missing = attach_urls(
                    rows.iter_mut()
                        .map(|r| (r.art_url.clone(), &mut r.art_path))
                        .collect(),
                );
                let total = page.total as i32;
                let mut guard = PAGE.lock().unwrap();
                if let Some(p) = guard.as_mut() {
                    if is_current_page_version(version) {
                        p.doc.albums.extend(rows);
                        p.doc.albums_total = total;
                        let doc = p.doc.clone();
                        drop(guard);
                        publish_page(&doc);
                    }
                }
                if !missing.is_empty() {
                    crate::spawn(async move {
                        refresh_loaded_page_art(version, missing).await;
                    });
                }
            }
            Err(e) => log::error!("[qbz-qt] search load-more albums failed: {e}"),
        },
        2 => match runtime
            .core()
            .search_tracks(&query, PAGE_SIZE, offset, search_type.as_deref())
            .await
        {
            Ok(page) => {
                let mut rows: Vec<TrackRow> = page
                    .items
                    .iter()
                    .filter(|t| !qbz_core::core::track_blacklisted(t, &bl, &abl))
                    .map(map_track)
                    .collect();
                let missing = attach_urls(
                    rows.iter_mut()
                        .map(|r| (r.art_url.clone(), &mut r.art_path))
                        .collect(),
                );
                let total = page.total as i32;
                let mut guard = PAGE.lock().unwrap();
                if let Some(p) = guard.as_mut() {
                    if is_current_page_version(version) {
                        p.doc.tracks.extend(rows);
                        p.doc.tracks_total = total;
                        let doc = p.doc.clone();
                        drop(guard);
                        publish_page(&doc);
                    }
                }
                if !missing.is_empty() {
                    crate::spawn(async move {
                        refresh_loaded_page_art(version, missing).await;
                    });
                }
            }
            Err(e) => log::error!("[qbz-qt] search load-more tracks failed: {e}"),
        },
        3 => match runtime
            .core()
            .search_artists(&query, PAGE_SIZE, offset, search_type.as_deref())
            .await
        {
            Ok(page) => {
                // Artist axis ONLY — the reference filters this category on
                // `bl` alone (`search.rs:1655`); an artist has no album id.
                let mut rows: Vec<ArtistRow> = page
                    .items
                    .iter()
                    .filter(|a| !bl.contains(&a.id))
                    .map(map_artist)
                    .collect();
                let missing = attach_urls(
                    rows.iter_mut()
                        .map(|r| (r.art_url.clone(), &mut r.art_path))
                        .collect(),
                );
                let total = page.total as i32;
                let mut guard = PAGE.lock().unwrap();
                if let Some(p) = guard.as_mut() {
                    if is_current_page_version(version) {
                        p.doc.artists.extend(rows);
                        p.doc.artists_total = total;
                        let doc = p.doc.clone();
                        drop(guard);
                        publish_page(&doc);
                    }
                }
                if !missing.is_empty() {
                    crate::spawn(async move {
                        refresh_loaded_page_art(version, missing).await;
                    });
                }
            }
            Err(e) => log::error!("[qbz-qt] search load-more artists failed: {e}"),
        },
        4 => match runtime
            .core()
            .search_playlists(&query, PAGE_SIZE, offset)
            .await
        {
            Ok(page) => {
                let mut rows: Vec<PlaylistRow> = page.items.iter().map(map_playlist).collect();
                let missing = attach_urls(
                    rows.iter_mut()
                        .map(|r| (r.art_url.clone(), &mut r.art_path))
                        .collect(),
                );
                let total = page.total as i32;
                let mut guard = PAGE.lock().unwrap();
                if let Some(p) = guard.as_mut() {
                    if is_current_page_version(version) {
                        p.doc.playlists.extend(rows);
                        p.doc.playlists_total = total;
                        let doc = p.doc.clone();
                        drop(guard);
                        publish_page(&doc);
                    }
                }
                if !missing.is_empty() {
                    crate::spawn(async move {
                        refresh_loaded_page_art(version, missing).await;
                    });
                }
            }
            Err(e) => log::error!("[qbz-qt] search load-more playlists failed: {e}"),
        },
        _ => {}
    }
}

/// searchType filter radios: re-query the three filterable categories and
/// REPLACE their lists (the filter takes effect on every tab).
pub async fn filter_changed(runtime: &Arc<AppRuntime<LoggingAdapter>>, index: i32) {
    let query = {
        let mut guard = PAGE.lock().unwrap();
        let Some(page) = guard.as_mut() else { return };
        page.doc.filter_index = index;
        let doc = page.doc.clone();
        publish_page(&doc);
        doc.query
    };
    if query.trim().is_empty() {
        return;
    }
    let search_type = search_type_for_filter(index);
    let version = next_page_version();
    // The reference implements the filter radios by calling `load_more(.., 0)`
    // per category and `replace_category` (`main.rs:9843-9856`), so these three
    // re-queries carry the SAME post-filter as `load_more` above.
    let (bl, abl) = blacklist_snapshots();

    // Albums.
    if let Ok(page) = runtime
        .core()
        .search_albums(&query, PAGE_SIZE, 0, search_type.as_deref())
        .await
    {
        let mut rows: Vec<CardRow> = page
            .items
            .iter()
            .filter(|a| !qbz_core::core::album_blacklisted(a, &bl, &abl))
            .map(map_album)
            .collect();
        let _ = attach_urls(
            rows.iter_mut()
                .map(|r| (r.art_url.clone(), &mut r.art_path))
                .collect(),
        );
        let total = page.total as i32;
        let mut guard = PAGE.lock().unwrap();
        if let Some(p) = guard.as_mut() {
            if is_current_page_version(version) {
                p.doc.albums = rows;
                p.doc.albums_total = total;
                let doc = p.doc.clone();
                drop(guard);
                publish_page(&doc);
            }
        }
    }
    // Tracks.
    if let Ok(page) = runtime
        .core()
        .search_tracks(&query, PAGE_SIZE, 0, search_type.as_deref())
        .await
    {
        let mut rows: Vec<TrackRow> = page
            .items
            .iter()
            .filter(|t| !qbz_core::core::track_blacklisted(t, &bl, &abl))
            .map(map_track)
            .collect();
        let _ = attach_urls(
            rows.iter_mut()
                .map(|r| (r.art_url.clone(), &mut r.art_path))
                .collect(),
        );
        let total = page.total as i32;
        let mut guard = PAGE.lock().unwrap();
        if let Some(p) = guard.as_mut() {
            if is_current_page_version(version) {
                p.doc.tracks = rows;
                p.doc.tracks_total = total;
                let doc = p.doc.clone();
                drop(guard);
                publish_page(&doc);
            }
        }
    }
    // Artists (the API takes the search_type too, 1:1 the Slint filter).
    if let Ok(page) = runtime
        .core()
        .search_artists(&query, PAGE_SIZE, 0, search_type.as_deref())
        .await
    {
        let mut rows: Vec<ArtistRow> = page
            .items
            .iter()
            .filter(|a| !bl.contains(&a.id))
            .map(map_artist)
            .collect();
        let _ = attach_urls(
            rows.iter_mut()
                .map(|r| (r.art_url.clone(), &mut r.art_path))
                .collect(),
        );
        let total = page.total as i32;
        let mut guard = PAGE.lock().unwrap();
        if let Some(p) = guard.as_mut() {
            if is_current_page_version(version) {
                p.doc.artists = rows.clone();
                p.doc.artists_total = total;
                p.doc.artists_carousel = rows;
                let doc = p.doc.clone();
                drop(guard);
                publish_page(&doc);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicated_top_result_adopts_the_visible_canonical_row() {
        let mut top = Some(CortRow {
            kind: "track".into(),
            id: "42".into(),
            title: "Same track".into(),
            art_url: String::new(),
            ..Default::default()
        });
        let tracks = vec![CortRow {
            kind: "track".into(),
            id: "42".into(),
            title: "Same track".into(),
            quality_detail: "24-bit / 96 kHz".into(),
            art_url: "https://static.qobuz.com/cover.jpg".into(),
            ..Default::default()
        }];

        canonicalize_top_result(&mut top, &[&tracks]);

        let top = top.expect("top result");
        assert_eq!(top.art_url, tracks[0].art_url);
        assert_eq!(top.quality_detail, tracks[0].quality_detail);
    }

    #[test]
    fn search_type_for_filter_maps_dropdown_index() {
        assert_eq!(search_type_for_filter(0), None);
        assert_eq!(search_type_for_filter(1), Some("MainArtist".to_string()));
        assert_eq!(search_type_for_filter(3), Some("Composer".to_string()));
        assert_eq!(search_type_for_filter(5), Some("ReleaseName".to_string()));
        assert_eq!(search_type_for_filter(99), None);
    }

    #[test]
    fn flat_indices_skip_zero_without_top_result() {
        let mut data = CortinillaData {
            query: "q".into(),
            top: None,
            sections: vec![CortSection {
                title: "Albums".into(),
                kind: "album".into(),
                has_more: false,
                rows: vec![
                    CortRow {
                        kind: "album".into(),
                        id: "1".into(),
                        ..Default::default()
                    },
                    CortRow {
                        kind: "album".into(),
                        id: "2".into(),
                        ..Default::default()
                    },
                ],
            }],
        };
        assign_flat_indices(&mut data);
        assert_eq!(data.sections[0].rows[0].flat_index, 1);
        assert_eq!(data.sections[0].rows[1].flat_index, 2);
    }

    #[test]
    fn flat_indices_top_is_zero() {
        let mut data = CortinillaData {
            query: "q".into(),
            top: Some(CortRow {
                kind: "artist".into(),
                id: "9".into(),
                ..Default::default()
            }),
            sections: vec![CortSection {
                title: "Albums".into(),
                kind: "album".into(),
                has_more: false,
                rows: vec![CortRow {
                    kind: "album".into(),
                    id: "1".into(),
                    ..Default::default()
                }],
            }],
        };
        assign_flat_indices(&mut data);
        assert_eq!(data.top.as_ref().unwrap().flat_index, 0);
        assert_eq!(data.sections[0].rows[0].flat_index, 1);
    }

    #[test]
    fn quality_label_formats() {
        assert_eq!(
            quality_label(Some(24), Some(96.0)),
            "Hi-Res 24-bit / 96 kHz"
        );
        assert_eq!(quality_label(Some(16), None), "CD 16-bit / 44.1 kHz");
        assert_eq!(quality_label(None, Some(192.0)), "");
        assert_eq!(mmss(5), "0:05");
        assert_eq!(mmss(225), "3:45");
        assert_eq!(tier(Some(24)), "hires");
        assert_eq!(tier(Some(16)), "cd");
        assert_eq!(tier(None), "");
    }

    // ---------------------------------------------------------------------
    // Immersive search controller (§3.4)
    // ---------------------------------------------------------------------

    fn imm_row(kind: &str, id: &str, source: &str) -> CortRow {
        CortRow {
            kind: kind.into(),
            id: id.into(),
            source: source.into(),
            ..Default::default()
        }
    }

    #[test]
    fn immersive_sections_order_caps_and_flat_indices() {
        // Contract §3.4 / search.rs:609-611,690-692: Artists, Albums,
        // Playlists IN THAT ORDER; caps 2/5/2; NO top result; NO track rows;
        // flat indices from 1 (main.rs:10491-10499).
        let artists: Vec<CortRow> = (0..4)
            .map(|i| imm_row("artist", &format!("ar{i}"), "qobuz"))
            .collect();
        let albums: Vec<CortRow> = (0..7)
            .map(|i| imm_row("album", &format!("al{i}"), "qobuz"))
            .collect();
        let playlists: Vec<CortRow> = (0..3)
            .map(|i| imm_row("playlist", &format!("pl{i}"), "qobuz"))
            .collect();
        let data = assemble_immersive_sections("q", artists, albums, playlists, (4, 7, 3));

        assert!(
            data.top.is_none(),
            "the immersive payload has NO top result"
        );
        let kinds: Vec<&str> = data.sections.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["artist", "album", "playlist"],
            "section order is Artists/Albums/Playlists"
        );
        assert_eq!(data.sections[0].rows.len(), 2, "artists cap 2");
        assert_eq!(data.sections[1].rows.len(), 5, "albums cap 5");
        assert_eq!(data.sections[2].rows.len(), 2, "playlists cap 2");
        assert!(
            data.sections.iter().all(|s| s.has_more),
            "totals exceed every cap"
        );
        // Flat indices run from 1, contiguous across sections.
        let flats: Vec<i32> = data
            .sections
            .iter()
            .flat_map(|s| s.rows.iter().map(|r| r.flat_index))
            .collect();
        assert_eq!(flats, (1..=9).collect::<Vec<i32>>());
        assert!(data
            .sections
            .iter()
            .flat_map(|s| s.rows.iter())
            .all(|r| r.kind != "track"));
    }

    #[test]
    fn immersive_sections_drop_empty_sections() {
        let data = assemble_immersive_sections(
            "q",
            vec![],
            vec![imm_row("album", "a1", "qobuz")],
            vec![],
            (0, 1, 0),
        );
        assert_eq!(data.sections.len(), 1);
        assert_eq!(data.sections[0].kind, "album");
        assert!(!data.sections[0].has_more);
        assert_eq!(data.sections[0].rows[0].flat_index, 1);
    }

    #[test]
    fn immersive_dispatch_table_matches_the_contract() {
        // Contract §3.4 round-2-verified mapping (main.rs:10614-10749).
        // Local album rows branch BEFORE the Qobuz match (the id is a group
        // key, not a numeric Qobuz id).
        assert_eq!(
            imm_dispatch("local", "album", "replace"),
            ImmDispatch::LocalPlay
        );
        assert_eq!(
            imm_dispatch("local", "album", "next"),
            ImmDispatch::LocalEnqueue("next".into())
        );
        assert_eq!(
            imm_dispatch("local", "album", "queue"),
            ImmDispatch::LocalEnqueue("queue".into())
        );
        // Qobuz replace arms.
        assert_eq!(
            imm_dispatch("qobuz", "album", "replace"),
            ImmDispatch::PlayAlbum
        );
        assert_eq!(
            imm_dispatch("qobuz", "playlist", "replace"),
            ImmDispatch::PlayPlaylist
        );
        assert_eq!(
            imm_dispatch("qobuz", "artist", "replace"),
            ImmDispatch::PlayArtist
        );
        // Qobuz next/queue arms (mode carried verbatim).
        assert_eq!(
            imm_dispatch("qobuz", "album", "next"),
            ImmDispatch::EnqueueAlbum("next".into())
        );
        assert_eq!(
            imm_dispatch("qobuz", "album", "queue"),
            ImmDispatch::EnqueueAlbum("queue".into())
        );
        assert_eq!(
            imm_dispatch("qobuz", "playlist", "next"),
            ImmDispatch::EnqueuePlaylist("next".into())
        );
        assert_eq!(
            imm_dispatch("qobuz", "playlist", "queue"),
            ImmDispatch::EnqueuePlaylist("queue".into())
        );
        assert_eq!(
            imm_dispatch("qobuz", "artist", "next"),
            ImmDispatch::EnqueueArtistTop("next".into())
        );
        assert_eq!(
            imm_dispatch("qobuz", "artist", "queue"),
            ImmDispatch::EnqueueArtistTop("queue".into())
        );
        // Unknown combinations are inert (no track rows exist in the payload;
        // an unknown action does nothing).
        assert_eq!(imm_dispatch("qobuz", "track", "replace"), ImmDispatch::None);
        assert_eq!(imm_dispatch("qobuz", "album", "later"), ImmDispatch::None);
        assert_eq!(imm_dispatch("local", "album", "later"), ImmDispatch::None);
    }

    #[test]
    fn immersive_selection_clamps_both_ends_no_wrap() {
        // main.rs:10524-10540: Down from -1 -> first row; Up from the first
        // row -> -1; both ends clamp (no wrap).
        let order = vec![1, 2, 3, 4];
        assert_eq!(
            imm_next_selection(&order, -1, 1),
            1,
            "Down from -1 lands on the first row"
        );
        assert_eq!(
            imm_next_selection(&order, -1, -1),
            -1,
            "Up from -1 stays at -1"
        );
        assert_eq!(
            imm_next_selection(&order, 1, -1),
            -1,
            "Up from the first row returns to -1"
        );
        assert_eq!(imm_next_selection(&order, 1, 1), 2);
        assert_eq!(
            imm_next_selection(&order, 4, 1),
            4,
            "Down on the last row clamps (no wrap)"
        );
        assert_eq!(imm_next_selection(&order, 3, -1), 2);
    }

    #[test]
    fn desktop_scroll_arithmetic_starts_at_zero_and_matches_the_layout() {
        // The DESKTOP layout (Cortinilla.qml): base 0 — the panel's 6px top
        // padding is a Flickable viewport margin, not content space. Then
        // top-result block = padTop 4 + label 22 + row 56 = 82, and per
        // section padTop 4 + header 24 = 28, 56 per row.
        //
        // This test exists because the base was 6.0 and nothing caught it:
        // the immersive twin had this test, the desktop arm did not.
        let mut data = CortinillaData {
            query: "q".into(),
            top: Some(imm_row("album", "top", "qobuz")),
            sections: vec![
                CortSection {
                    title: "Albums".into(),
                    kind: "album".into(),
                    has_more: false,
                    rows: vec![
                        imm_row("album", "1", "qobuz"),
                        imm_row("album", "2", "qobuz"),
                    ],
                },
                CortSection {
                    title: "Artists".into(),
                    kind: "artist".into(),
                    has_more: false,
                    rows: vec![imm_row("artist", "3", "qobuz")],
                },
            ],
        };
        assign_flat_indices(&mut data);
        assert_eq!(
            flat_index_content_y(&data, 0),
            26.0,
            "the top result sits at 4 + 22 — NOT 32, which is what the 6px \
             viewport margin used to add"
        );
        assert_eq!(
            flat_index_content_y(&data, 1),
            122.0,
            "first section row: 94 (top block) + 28 (section head)"
        );
        assert_eq!(flat_index_content_y(&data, 2), 190.0, "122 + one 68px row");
        assert_eq!(
            flat_index_content_y(&data, 3),
            286.0,
            "94 + 28 + 2*68 + the second section's 28"
        );
        assert_eq!(
            flat_index_content_y(&data, 99),
            0.0,
            "an unknown index falls to the top"
        );
    }

    #[test]
    fn desktop_scroll_arithmetic_without_a_top_result() {
        // No top-result block: the first section row starts at 28, so the
        // 6px base would have been visible here as 34.
        let mut data = CortinillaData {
            query: "q".into(),
            top: None,
            sections: vec![CortSection {
                title: "Albums".into(),
                kind: "album".into(),
                has_more: false,
                rows: vec![imm_row("album", "1", "qobuz")],
            }],
        };
        assign_flat_indices(&mut data);
        // Flat indices still START AT 1 with no top result (C7 convention).
        assert_eq!(flat_index_content_y(&data, 1), 28.0);
    }

    #[test]
    fn immersive_scroll_arithmetic_matches_the_no_top_layout() {
        // main.rs:10530-10547: per-section padTop 4 + header 24 (= 28), 56
        // per row, NO top-result block. Section 1 has 2 rows, section 2 has 1.
        let mut data = CortinillaData {
            query: "q".into(),
            top: None,
            sections: vec![
                CortSection {
                    title: "Artists".into(),
                    kind: "artist".into(),
                    has_more: false,
                    rows: vec![
                        imm_row("artist", "1", "qobuz"),
                        imm_row("artist", "2", "qobuz"),
                    ],
                },
                CortSection {
                    title: "Albums".into(),
                    kind: "album".into(),
                    has_more: false,
                    rows: vec![imm_row("album", "3", "qobuz")],
                },
            ],
        };
        assign_flat_indices(&mut data);
        assert_eq!(
            imm_scroll_y(&data, -1),
            0.0,
            "no selection scrolls to the top"
        );
        assert_eq!(imm_scroll_y(&data, 1), 28.0, "first section header block");
        assert_eq!(imm_scroll_y(&data, 2), 84.0, "28 + one 56px row");
        assert_eq!(
            imm_scroll_y(&data, 3),
            168.0,
            "28 + 2*56 + the second section's 28"
        );
        assert_eq!(imm_scroll_y(&data, 99), 0.0, "unknown index falls to 0");
    }

    #[test]
    fn local_caps_profiles_and_fetch_limit() {
        // search.rs:717-746: normal 3/2/3, expanded 8/4/8; fetch_limit =
        // max(albums, tracks) * 12 + 40.
        assert_eq!(
            crate::search_local::LocalCaps::for_session(false),
            crate::search_local::LocalCaps {
                albums: 3,
                artists: 2,
                tracks: 3
            }
        );
        assert_eq!(
            crate::search_local::LocalCaps::for_session(true),
            crate::search_local::LocalCaps {
                albums: 8,
                artists: 4,
                tracks: 8
            }
        );
        assert_eq!(crate::search_local::LocalCaps::NORMAL.fetch_limit(), 76);
        assert_eq!(crate::search_local::LocalCaps::EXPANDED.fetch_limit(), 136);
    }
}
