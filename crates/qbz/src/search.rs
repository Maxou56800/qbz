//! Search results controller.
//!
//! Three stages, mirroring `album.rs`: `load_search` fetches a combined
//! search through `QbzCore` on a worker thread, `map_*` turns the domain
//! types into plain `Send` rows (the unit-tested layer), and
//! `apply_search` writes the `SearchState` global on the Slint event loop.

use std::collections::HashSet;
use std::sync::Arc;

use qbz_app::shell::AppRuntime;
use qbz_core::FrontendAdapter;
use qbz_models::{Album, Artist, MostPopularItem, Playlist, SearchAllResults, Track};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::artwork::{ArtworkJob, ArtworkTarget};
use crate::{
    AlbumCardItem, AppWindow, CortinillaRow as CortinillaRowItem,
    CortinillaSection as CortinillaSectionItem, ExternalRecoState, ForYouState, HomeState,
    LabelState, SearchPlaylistItem, SearchState, SlimItem, TrackItem,
};

thread_local! {
    /// Monotonic search-attempt counter. Each `navigate_search` captures the
    /// current value; a stale async load whose version is no longer current
    /// must not overwrite a newer search's results. UI thread only.
    static SEARCH_VERSION: std::cell::Cell<u64> = std::cell::Cell::new(0);

    /// Monotonic cortinilla-attempt counter — SEPARATE from `SEARCH_VERSION`.
    /// The live dropdown fires far more often than the results page (one bump
    /// per debounced keystroke + the instant cached paint), and a stale
    /// revalidation must not overwrite a newer query's dropdown. UI thread only.
    static CORTINILLA_VERSION: std::cell::Cell<u64> = std::cell::Cell::new(0);

    /// Monotonic immersive-search-attempt counter — SEPARATE from both the
    /// results-page (`SEARCH_VERSION`) and the main-header (`CORTINILLA_VERSION`)
    /// counters, so the in-immersive dropdown's stale-load guard never collides
    /// with the main cortinilla's (both surfaces can be open across a session).
    /// UI thread only.
    static IMMERSIVE_SEARCH_VERSION: std::cell::Cell<u64> = std::cell::Cell::new(0);
}

/// Bump the search version and return the new value.
pub fn next_search_version() -> u64 {
    SEARCH_VERSION.with(|c| {
        let v = c.get() + 1;
        c.set(v);
        v
    })
}

/// Whether `version` is still the most recent search attempt.
pub fn is_current_version(version: u64) -> bool {
    SEARCH_VERSION.with(|c| c.get() == version)
}

/// Bump the cortinilla version and return the new value.
pub fn next_cortinilla_version() -> u64 {
    CORTINILLA_VERSION.with(|c| {
        let v = c.get() + 1;
        c.set(v);
        v
    })
}

/// Whether `version` is still the most recent cortinilla attempt.
pub fn is_current_cortinilla_version(version: u64) -> bool {
    CORTINILLA_VERSION.with(|c| c.get() == version)
}

/// Bump the immersive-search version and return the new value.
pub fn next_immersive_search_version() -> u64 {
    IMMERSIVE_SEARCH_VERSION.with(|c| {
        let v = c.get() + 1;
        c.set(v);
        v
    })
}

/// Whether `version` is still the most recent immersive-search attempt.
pub fn is_current_immersive_search_version(version: u64) -> bool {
    IMMERSIVE_SEARCH_VERSION.with(|c| c.get() == version)
}

// ==================== Plain (Send) row types ====================

/// An album result row, before it becomes a Slint `AlbumCardItem`.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumRow {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: String,
    pub genre: String,
    pub year: String,
    pub quality_tier: String,
    pub quality_label: String,
    pub artwork_url: String,
}

/// A track result row, before it becomes a Slint `TrackItem`.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackRowData {
    pub id: String,
    pub title: String,
    pub artist: String,
    /// Performer id for the clickable artist link ("" = plain text).
    pub artist_id: String,
    /// Album id for the clickable album link ("" = plain text).
    pub album_id: String,
    pub duration: String,
    pub quality_tier: String,
    /// Detailed quality label, e.g. "Hi-Res 24-bit / 192 kHz". Used by the
    /// most-popular track hero (shown as text instead of an icon badge).
    pub quality_label: String,
    /// Exact bit-depth / sample-rate line, e.g. "24-bit / 192 kHz" — feeds the
    /// track-row quality badge (no tier prefix, unlike `quality_label`).
    pub quality_detail: String,
    pub explicit: bool,
    pub artwork_url: String,
}

/// An artist result row, before it becomes a Slint `SlimItem`.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistRow {
    pub id: String,
    pub name: String,
    pub subtitle: String,
    pub artwork_url: String,
    /// Whether the user already follows (favorites) this artist.
    pub following: bool,
}

/// A playlist result row, before it becomes a Slint `SearchPlaylistItem`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistRow {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    /// Up to four distinct cover URLs for the collage.
    pub cover_urls: Vec<String>,
    /// Ownership signals for the card overlay/menu (owned → favorite; foreign
    /// Qobuz → follow + copy). `is_owned` is authoritative (owner.id ==
    /// current user); `is_following`/`is_copied` are best-effort per source
    /// (favorites seeds `is_following` from the followed split; other list
    /// surfaces leave them false — the action still works id-scoped).
    pub is_owned: bool,
    pub is_following: bool,
    pub is_copied: bool,
}

/// The most-popular hero entry.
#[derive(Debug, Clone, PartialEq)]
pub enum MostPopularRow {
    None,
    Album(AlbumRow),
    Artist(ArtistRow),
    Track(TrackRowData),
}

/// The full result of a combined search, as plain `Send` data.
pub struct SearchData {
    pub query: String,
    pub albums: Vec<AlbumRow>,
    pub tracks: Vec<TrackRowData>,
    pub artists: Vec<ArtistRow>,
    pub playlists: Vec<PlaylistRow>,
    pub albums_total: u32,
    pub tracks_total: u32,
    pub artists_total: u32,
    pub playlists_total: u32,
    pub most_popular: MostPopularRow,
}

// ==================== Cortinilla (live dropdown) row types ====================

/// One plain (`Send`) cortinilla row, before it becomes a Slint
/// `CortinillaRow`. `source` selects the click seam ("qobuz" media/nav vs
/// "local" play); `kind` is the navigable category. `flat_index` is the stable
/// 0-based selection index across the WHOLE navigable list (top-result = 0,
/// then section rows in display order), assigned by `map_search_all_to_cortinilla`.
#[derive(Debug, Clone, PartialEq)]
pub struct CortRow {
    pub kind: String,
    pub id: String,
    pub source: String,
    pub title: String,
    pub subtitle: String,
    pub artwork_url: String,
    pub flat_index: usize,
}

/// One labelled cortinilla section (e.g. "Artists", "Albums").
#[derive(Debug, Clone, PartialEq)]
pub struct CortSection {
    pub title: String,
    pub kind: String,
    pub rows: Vec<CortRow>,
    pub has_more: bool,
}

/// The full cortinilla payload, as plain `Send` data.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CortinillaData {
    pub query: String,
    pub top: Option<CortRow>,
    pub sections: Vec<CortSection>,
}

/// How many rows each cortinilla category shows before "View more".
/// Per-category row caps in the cortinilla. Artists are rarely opened past the
/// first hit, so they get the smallest cap and the freed space goes to albums
/// (the most-scanned category). Tracks/playlists keep the default 3.
const CORTINILLA_CAP_ALBUMS: usize = 5;
const CORTINILLA_CAP_ARTISTS: usize = 2;
const CORTINILLA_CAP_TRACKS: usize = 3;
const CORTINILLA_CAP_PLAYLISTS: usize = 3;

// ==================== Pure helpers ====================

/// 24-bit and up is Hi-Res, anything else with depth info is CD-quality.
fn tier(bit_depth: Option<u32>) -> &'static str {
    match bit_depth {
        Some(depth) if depth >= 24 => "hires",
        Some(_) => "cd",
        None => "",
    }
}

/// Quality-badge tooltip, e.g. "Hi-Res 24-bit / 96 kHz". Empty when no
/// quality info is available.
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

/// `m:ss` track duration.
fn mmss(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// First four characters of an ISO date, or empty.
fn year_of(date: Option<&str>) -> String {
    date.and_then(|d| d.get(0..4)).unwrap_or("").to_string()
}

/// Up to four distinct cover URLs for a playlist collage. Qobuz returns
/// pre-built cover lists in `images300` / `images150` / `images`; the
/// highest-resolution non-empty list wins.
fn playlist_cover_urls(playlist: &Playlist) -> Vec<String> {
    let source = [
        &playlist.images300,
        &playlist.images150,
        &playlist.images,
    ]
    .into_iter()
    .flatten()
    .find(|v| !v.is_empty());

    let mut out: Vec<String> = Vec::new();
    if let Some(list) = source {
        for url in list {
            if !url.is_empty() && !out.contains(url) {
                out.push(url.clone());
            }
            if out.len() == 4 {
                break;
            }
        }
    }
    out
}

// ==================== Mappers (unit-tested) ====================

pub fn map_album(album: Album) -> AlbumRow {
    AlbumRow {
        id: album.id,
        title: crate::album_map::format_album_title(&album.title, album.version.as_deref()),
        artist: album.artist.name,
        artist_id: album.artist.id.to_string(),
        genre: album
            .genre
            .map(|g| g.name)
            .filter(|n| !n.is_empty())
            .unwrap_or_default(),
        year: year_of(album.release_date_original.as_deref()),
        quality_tier: tier(album.maximum_bit_depth).to_string(),
        quality_label: quality_label(album.maximum_bit_depth, album.maximum_sampling_rate),
        artwork_url: album.image.best().cloned().unwrap_or_default(),
    }
}

pub fn map_track(track: Track) -> TrackRowData {
    let mut title = track.title;
    if let Some(version) = track.version.as_ref().filter(|v| !v.is_empty()) {
        title = format!("{title} ({version})");
    }
    let artwork_url = track
        .album
        .as_ref()
        .and_then(|a| a.image.best().cloned())
        .unwrap_or_default();
    let album_id = track.album.as_ref().map(|a| a.id.clone()).unwrap_or_default();
    let (artist, artist_id) = track
        .performer
        .map(|p| (p.name, p.id.to_string()))
        .unwrap_or_default();
    TrackRowData {
        id: track.id.to_string(),
        title,
        artist,
        artist_id,
        album_id,
        duration: mmss(track.duration),
        quality_tier: tier(track.maximum_bit_depth).to_string(),
        quality_label: quality_label(track.maximum_bit_depth, track.maximum_sampling_rate),
        quality_detail: crate::quality::detail(
            track.maximum_bit_depth,
            track.maximum_sampling_rate,
        ),
        explicit: track.parental_warning,
        artwork_url,
    }
}

pub fn map_artist(artist: &Artist, following: bool) -> ArtistRow {
    ArtistRow {
        id: artist.id.to_string(),
        name: artist.name.clone(),
        subtitle: match artist.albums_count {
            Some(n) if n > 0 => qbz_i18n::tf("{} album", "{} albums", n as i64, &[&n.to_string()]),
            _ => String::new(),
        },
        artwork_url: artist
            .image
            .as_ref()
            .and_then(|i| i.best().cloned())
            .unwrap_or_default(),
        following,
    }
}

pub fn map_playlist(playlist: Playlist) -> PlaylistRow {
    let cover_urls = playlist_cover_urls(&playlist);
    let mut subtitle = playlist.owner.name.clone();
    if playlist.tracks_count > 0 {
        let count = playlist.tracks_count;
        let tracks_label = qbz_i18n::tf("{} track", "{} tracks", count as i64, &[&count.to_string()]);
        if subtitle.is_empty() {
            subtitle = tracks_label;
        } else {
            subtitle = format!("{}   •   {}", subtitle, tracks_label);
        }
    }
    let is_owned = crate::library_db::current_user_id()
        .map(|uid| uid == playlist.owner.id)
        .unwrap_or(false);
    PlaylistRow {
        id: playlist.id.to_string(),
        title: playlist.name,
        subtitle,
        cover_urls,
        is_owned,
        is_following: false,
        is_copied: false,
    }
}

fn map_most_popular(item: Option<MostPopularItem>, favorite_artists: &HashSet<u64>) -> MostPopularRow {
    match item {
        Some(MostPopularItem::Albums(a)) => MostPopularRow::Album(map_album(a)),
        Some(MostPopularItem::Artists(a)) => {
            let following = favorite_artists.contains(&a.id);
            MostPopularRow::Artist(map_artist(&a, following))
        }
        Some(MostPopularItem::Tracks(t)) => MostPopularRow::Track(map_track(t)),
        None => MostPopularRow::None,
    }
}

/// Map a combined-search result into plain `Send` data. `favorite_artists`
/// is the set of artist ids the user already follows.
pub fn map_search_all(
    query: &str,
    results: SearchAllResults,
    favorite_artists: &HashSet<u64>,
) -> SearchData {
    let artists: Vec<ArtistRow> = results
        .artists
        .items
        .iter()
        .map(|a| map_artist(a, favorite_artists.contains(&a.id)))
        .collect();
    let most_popular = map_most_popular(results.most_popular, favorite_artists);
    // Dedupe used to drop the top-result artist from the artists list
    // here, but the Artists tab does not show the Most-popular hero —
    // it should keep the artist. The dedupe now lives at `apply_search`
    // where the carousel-only `artists_carousel` is built.
    SearchData {
        query: query.to_string(),
        albums_total: results.albums.total,
        tracks_total: results.tracks.total,
        artists_total: results.artists.total,
        playlists_total: results.playlists.total,
        albums: results.albums.items.into_iter().map(map_album).collect(),
        tracks: results.tracks.items.into_iter().map(map_track).collect(),
        artists,
        playlists: results.playlists.items.into_iter().map(map_playlist).collect(),
        most_popular,
    }
}

// ==================== Cortinilla mapping ====================

/// Build the cortinilla payload from a combined-search result.
///
/// Section order (display + flat-index order): **Top result**, then **Albums,
/// Artists, Tracks, Playlists** (spec §6.2.3). Per-category caps (albums 5,
/// artists 2, tracks/playlists 3) — artists are rarely opened past the first
/// hit, so albums get the freed space; `has_more` is set when the category's
/// reported total exceeds the rows shown.
///
/// Intra-category order applies the qbz-app learned ranking
/// (`search_service::rank_within`) BEFORE truncation, so a frequently-opened
/// entity floats to the top of its section.
///
/// Top result: if `top_kind_id` (the learned `(kind, id)` for this query) is
/// `Some` and matches a mapped row, that row is promoted; otherwise the
/// `most_popular` hero is used; otherwise the first artist, then the first
/// album. The promoted entity is NOT removed from its section (the cortinilla
/// is small; a one-row dup is acceptable and matches the results page, which
/// keeps the artist in the Artists tab).
pub fn map_search_all_to_cortinilla(
    query: &str,
    results: &SearchAllResults,
    top_kind_id: Option<(String, String)>,
) -> CortinillaData {
    // Map each category to CortRow (source = "qobuz"), apply ranking, truncate.
    let rank_and_take = |kind: &str, mut rows: Vec<CortRow>, cap: usize| -> (Vec<CortRow>, usize) {
        let total = rows.len();
        crate::search_service::rank_within(query, kind, &mut rows, |r| r.id.clone());
        rows.truncate(cap);
        (rows, total)
    };

    let to_artist_row = |a: &Artist| CortRow {
        kind: "artist".into(),
        id: a.id.to_string(),
        source: "qobuz".into(),
        title: a.name.clone(),
        subtitle: map_artist(a, false).subtitle,
        artwork_url: a
            .image
            .as_ref()
            .and_then(|i| i.best().cloned())
            .unwrap_or_default(),
        flat_index: 0,
    };
    let to_album_row = |al: &Album| {
        let m = map_album(al.clone());
        CortRow {
            kind: "album".into(),
            id: m.id,
            source: "qobuz".into(),
            title: m.title,
            subtitle: m.artist,
            artwork_url: m.artwork_url,
            flat_index: 0,
        }
    };
    let to_track_row = |t: &Track| {
        let m = map_track(t.clone());
        CortRow {
            kind: "track".into(),
            id: m.id,
            source: "qobuz".into(),
            title: m.title,
            subtitle: m.artist,
            artwork_url: m.artwork_url,
            flat_index: 0,
        }
    };
    let to_playlist_row = |p: &Playlist| {
        let m = map_playlist(p.clone());
        CortRow {
            kind: "playlist".into(),
            id: m.id,
            source: "qobuz".into(),
            title: m.title,
            subtitle: m.subtitle,
            artwork_url: m.cover_urls.first().cloned().unwrap_or_default(),
            flat_index: 0,
        }
    };

    // Borrow the closures (`&F: Fn` when `F: Fn`) so they are not consumed here
    // — the top-result fallback below reuses the same closures.
    let artist_rows: Vec<CortRow> = results.artists.items.iter().map(&to_artist_row).collect();
    let album_rows: Vec<CortRow> = results.albums.items.iter().map(&to_album_row).collect();
    let track_rows: Vec<CortRow> = results.tracks.items.iter().map(&to_track_row).collect();
    let playlist_rows: Vec<CortRow> =
        results.playlists.items.iter().map(&to_playlist_row).collect();

    let (artists, _) = rank_and_take("artist", artist_rows, CORTINILLA_CAP_ARTISTS);
    let (albums, _) = rank_and_take("album", album_rows, CORTINILLA_CAP_ALBUMS);
    let (tracks, _) = rank_and_take("track", track_rows, CORTINILLA_CAP_TRACKS);
    let (playlists, _) = rank_and_take("playlist", playlist_rows, CORTINILLA_CAP_PLAYLISTS);

    // Pick the top result. The promoted row is identified by (kind, id) so it
    // can be located across the already-mapped section rows; if the learned
    // pick is not present in the (truncated) sections, fall back to mapping the
    // raw catalog entry directly so it still shows even when ranked out.
    let find_in =
        |kind: &str, id: &str| -> Option<CortRow> {
            let sect = match kind {
                "artist" => &artists,
                "album" => &albums,
                "track" => &tracks,
                "playlist" => &playlists,
                _ => return None,
            };
            sect.iter().find(|r| r.id == id).cloned()
        };

    let top: Option<CortRow> = top_kind_id
        .and_then(|(kind, id)| {
            // Prefer a row already mapped; else map the raw catalog entry.
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
            // Reuse the existing most-popular shape as a fallback top result.
            Some(MostPopularItem::Artists(a)) => Some(to_artist_row(a)),
            Some(MostPopularItem::Albums(a)) => Some(to_album_row(a)),
            Some(MostPopularItem::Tracks(t)) => Some(to_track_row(t)),
            None => None,
        })
        .or_else(|| artists.first().cloned())
        .or_else(|| albums.first().cloned());

    // Assemble sections in display order (spec §6.2.3): Albums, Artists,
    // Tracks, Playlists. The local "on this device" sections are appended LAST,
    // outside this function (see `append_local_sections`).
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
    push_section(&qbz_i18n::t("Albums"), "album", albums, results.albums.total);
    push_section(&qbz_i18n::t("Artists"), "artist", artists, results.artists.total);
    push_section(&qbz_i18n::t("Tracks"), "track", tracks, results.tracks.total);
    push_section(&qbz_i18n::t("Playlists"), "playlist", playlists, results.playlists.total);

    // Assign the flat selection index across the whole navigable list:
    // top-result = 0, then every section's rows in display order.
    let mut data = CortinillaData {
        query: query.to_string(),
        top,
        sections,
    };
    assign_flat_indices(&mut data);
    data
}

/// Per-category caps for the IMMERSIVE search cortinilla (owner sketch).
const IMMERSIVE_CAP_ARTISTS: usize = 2;
const IMMERSIVE_CAP_ALBUMS: usize = 5;
const IMMERSIVE_CAP_PLAYLISTS: usize = 2;

/// Immersive-search variant of [`map_search_all_to_cortinilla`]: **Albums /
/// Artists / Playlists ONLY** (no tracks, no local, no top-result hero —
/// immersive has no navigation, so selecting a row acts on the queue instead).
/// Section order matches the owner sketch: Artists, Albums, Playlists. Intra-
/// category order still applies the learned ranking before truncation.
pub fn map_search_all_to_immersive(query: &str, results: &SearchAllResults) -> CortinillaData {
    let to_artist_row = |a: &Artist| CortRow {
        kind: "artist".into(),
        id: a.id.to_string(),
        source: "qobuz".into(),
        title: a.name.clone(),
        subtitle: map_artist(a, false).subtitle,
        artwork_url: a
            .image
            .as_ref()
            .and_then(|i| i.best().cloned())
            .unwrap_or_default(),
        flat_index: 0,
    };
    let to_album_row = |al: &Album| {
        let m = map_album(al.clone());
        CortRow {
            kind: "album".into(),
            id: m.id,
            source: "qobuz".into(),
            title: m.title,
            subtitle: m.artist,
            artwork_url: m.artwork_url,
            flat_index: 0,
        }
    };
    let to_playlist_row = |p: &Playlist| {
        let m = map_playlist(p.clone());
        CortRow {
            kind: "playlist".into(),
            id: m.id,
            source: "qobuz".into(),
            title: m.title,
            subtitle: m.subtitle,
            artwork_url: m.cover_urls.first().cloned().unwrap_or_default(),
            flat_index: 0,
        }
    };

    let take = |kind: &str, mut rows: Vec<CortRow>, cap: usize| -> Vec<CortRow> {
        crate::search_service::rank_within(query, kind, &mut rows, |r| r.id.clone());
        rows.truncate(cap);
        rows
    };

    let artists = take(
        "artist",
        results.artists.items.iter().map(to_artist_row).collect(),
        IMMERSIVE_CAP_ARTISTS,
    );
    let albums = take(
        "album",
        results.albums.items.iter().map(to_album_row).collect(),
        IMMERSIVE_CAP_ALBUMS,
    );
    let playlists = take(
        "playlist",
        results.playlists.items.iter().map(to_playlist_row).collect(),
        IMMERSIVE_CAP_PLAYLISTS,
    );

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
    push(&qbz_i18n::t("Artists"), "artist", artists, results.artists.total);
    push(&qbz_i18n::t("Albums"), "album", albums, results.albums.total);
    push(&qbz_i18n::t("Playlists"), "playlist", playlists, results.playlists.total);

    let mut data = CortinillaData {
        query: query.to_string(),
        top: None,
        sections,
    };
    assign_flat_indices(&mut data);
    data
}

/// Per-section caps for the LOCAL cortinilla sections. Two profiles: the NORMAL
/// (online + signed-in) profile keeps the on-device block compact since the
/// Qobuz catalog dominates the dropdown; the EXPANDED profile (offline OR not
/// signed in, so the cortinilla is local-only) turns it into a small on-device
/// browser with more rows per section.
#[derive(Debug, Clone, Copy)]
pub struct LocalCaps {
    pub albums: usize,
    pub artists: usize,
    pub tracks: usize,
}

impl LocalCaps {
    /// Normal profile (Qobuz present): compact on-device block.
    const NORMAL: LocalCaps = LocalCaps {
        albums: 3,
        artists: 2,
        tracks: 3,
    };
    /// Expanded profile (offline / not signed in → local-only dropdown).
    const EXPANDED: LocalCaps = LocalCaps {
        albums: 8,
        artists: 4,
        tracks: 8,
    };

    /// Pick the profile for the current session state. `expand` is true when the
    /// session is offline OR unauthenticated (the cortinilla has no Qobuz half).
    pub fn for_session(expand: bool) -> LocalCaps {
        if expand {
            Self::EXPANDED
        } else {
            Self::NORMAL
        }
    }

    /// How many raw local TRACK rows to fetch so the grouped album/artist
    /// sections can be filled. Albums/artists are derived by grouping tracks, so
    /// a single album can swallow many rows — over-fetch well beyond the shown
    /// caps to surface enough distinct groups.
    fn fetch_limit(self) -> u64 {
        ((self.albums.max(self.tracks) * 12) + 40) as u64
    }
}

/// The artwork key for a local/Plex cortinilla row: the RAW path, so the search
/// artwork dispatcher (`artwork::spawn_search_loads`) can route it by scheme —
/// `/library/…` & `/photo/…` → PlexThumb, http(s) → Qobuz CDN, anything else
/// (an absolute filesystem path) → `LocalFile` (decoded with `fs::read`, so NO
/// `file://` prefix). A stray `file://` is stripped for the same reason.
fn local_artwork_url(path: Option<&str>) -> String {
    path.map(|p| p.strip_prefix("file://").unwrap_or(p).to_string())
        .unwrap_or_default()
}

/// The canonical "artist" attributed to a local track for grouping: the
/// album-artist tag when present, else the track performer. Mirrors the album-
/// grouping helper in `local_library` so cortinilla artists line up with the
/// LocalLibrary Artists tab (which `open_local_artist` selects by NAME).
fn local_album_artist(t: &qbz_library::LocalTrack) -> String {
    t.album_artist
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| t.artist.clone())
}

/// Group local TRACK rows into local ALBUM cortinilla rows (`source = "local"`,
/// `kind = "album"`). Grouped by `album_group_key` in first-seen order (the DB
/// returns rows by match relevance). `id` is the group key — the click router
/// opens the LocalAlbum view with it (`navigate_local_album`). Returns the
/// capped rows plus whether more distinct albums existed than shown.
fn derive_local_album_rows(rows: &[qbz_library::LocalTrack], cap: usize) -> (Vec<CortRow>, bool) {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<CortRow> = Vec::new();
    let mut total = 0usize;
    for t in rows {
        let key = t.album_group_key.clone();
        if key.is_empty() || !seen.insert(key.clone()) {
            continue;
        }
        total += 1;
        if out.len() >= cap {
            continue; // keep counting for an honest has_more
        }
        let title = if t.album_group_title.is_empty() {
            t.album.clone()
        } else {
            t.album_group_title.clone()
        };
        out.push(CortRow {
            kind: "album".into(),
            id: key,
            source: "local".into(),
            title,
            subtitle: local_album_artist(t),
            artwork_url: local_artwork_url(t.artwork_path.as_deref()),
            flat_index: 0,
        });
    }
    let has_more = total > out.len();
    (out, has_more)
}

/// Group local TRACK rows into local ARTIST cortinilla rows (`source = "local"`,
/// `kind = "artist"`). Grouped by the canonical album-artist, case-insensitively,
/// in first-seen order. Local artists have no id — the click router opens the
/// LocalLibrary Artists tab by NAME (the row `title`), so `id` is left empty.
fn derive_local_artist_rows(rows: &[qbz_library::LocalTrack], cap: usize) -> (Vec<CortRow>, bool) {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<CortRow> = Vec::new();
    let mut total = 0usize;
    for t in rows {
        let name = local_album_artist(t);
        if name.is_empty() || !seen.insert(name.to_lowercase()) {
            continue;
        }
        total += 1;
        if out.len() >= cap {
            continue;
        }
        out.push(CortRow {
            kind: "artist".into(),
            id: String::new(),
            source: "local".into(),
            title: name,
            subtitle: String::new(),
            artwork_url: local_artwork_url(t.artwork_path.as_deref()),
            flat_index: 0,
        });
    }
    let has_more = total > out.len();
    (out, has_more)
}

/// Map one `LocalTrack` to a cortinilla `CortRow` tagged `source = "local"`.
///
/// `kind = "track"` (it navigates/plays as a track), but the click router keys
/// off `source == "local"` to play it through the LOCAL seam
/// (`playback::play_local_tracks`) rather than the Qobuz media-action. The id
/// is the library row id (`LocalTrack::id`) — the click router resolves the
/// concrete `LocalTrack` back from the per-query snapshot, NOT from this id.
///
/// Artwork prefixing mirrors `playback::local_queue_track` /
/// `local_library::map_local_track`: a raw fs path is `file://`-prefixed unless
/// it already carries a `file://` scheme (a Plex thumb never reaches here — the
/// local-library DB query returns user files / offline copies only, never the
/// Plex cache).
fn map_local_track_to_cort_row(t: &qbz_library::LocalTrack) -> CortRow {
    let artwork_url = local_artwork_url(t.artwork_path.as_deref());
    // Subtitle: "artist · album" when both are present, else whichever exists.
    let subtitle = match (t.artist.is_empty(), t.album.is_empty()) {
        (false, false) => format!("{} · {}", t.artist, t.album),
        (false, true) => t.artist.clone(),
        (true, false) => t.album.clone(),
        (true, true) => String::new(),
    };
    CortRow {
        kind: "track".into(),
        id: t.id.to_string(),
        source: "local".into(),
        title: t.title.clone(),
        subtitle,
        artwork_url,
        flat_index: 0,
    }
}

/// Append the local "on this device" sections to a MAIN cortinilla payload,
/// placed LAST (after every Qobuz category, per D1/D2 — local results live ONLY
/// in the cortinilla). Three sections in display order: **Albums**, **Artists**,
/// **Tracks** (mirrors the Qobuz section order), each capped per [`LocalCaps`].
/// Albums/artists are DERIVED by grouping the local track rows. Section `kind`s
/// are `local-album` / `local-artist` / `local` so the "View more" router opens
/// the matching LocalLibrary tab; per-row `kind` stays album/artist/track so the
/// thumbnail shape + the row click router route correctly. No-op when `rows` is
/// empty. Re-runs `assign_flat_indices` so the local rows get contiguous flat
/// indices AFTER the Qobuz sections.
pub fn append_local_sections(
    data: &mut CortinillaData,
    rows: &[qbz_library::LocalTrack],
    caps: LocalCaps,
) {
    if rows.is_empty() {
        return;
    }
    let (album_rows, albums_more) = derive_local_album_rows(rows, caps.albums);
    if !album_rows.is_empty() {
        data.sections.push(CortSection {
            title: qbz_i18n::t("Albums on Local Library"),
            kind: "local-album".to_string(),
            rows: album_rows,
            has_more: albums_more,
        });
    }
    let (artist_rows, artists_more) = derive_local_artist_rows(rows, caps.artists);
    if !artist_rows.is_empty() {
        data.sections.push(CortSection {
            title: qbz_i18n::t("Artists on Local Library"),
            kind: "local-artist".to_string(),
            rows: artist_rows,
            has_more: artists_more,
        });
    }
    let track_rows: Vec<CortRow> = rows
        .iter()
        .take(caps.tracks)
        .map(map_local_track_to_cort_row)
        .collect();
    if !track_rows.is_empty() {
        let shown = track_rows.len();
        data.sections.push(CortSection {
            title: qbz_i18n::t("On Local Library"),
            kind: "local".to_string(),
            rows: track_rows,
            has_more: rows.len() > shown,
        });
    }
    assign_flat_indices(data);
}

/// Append the local ALBUM section to an IMMERSIVE cortinilla payload (immersive
/// shows albums ONLY — selecting one queues it per the configured action). Rows
/// are derived local albums tagged `kind = "album"` / `source = "local"`,
/// `kind`-tagged section `local-album`. No "View more" in immersive, so the
/// `has_more` flag is carried but unused by the UI. No-op when `rows` is empty.
fn append_immersive_local_albums(
    data: &mut CortinillaData,
    rows: &[qbz_library::LocalTrack],
    cap: usize,
) {
    if rows.is_empty() {
        return;
    }
    let (album_rows, has_more) = derive_local_album_rows(rows, cap);
    if album_rows.is_empty() {
        return;
    }
    data.sections.push(CortSection {
        title: qbz_i18n::t("Albums on Local Library"),
        kind: "local-album".to_string(),
        rows: album_rows,
        has_more,
    });
    assign_flat_indices(data);
}

/// Fetch up to `limit` local-library tracks matching `query`, off the UI thread
/// (the rusqlite read is sync + blocking, so it runs inside `spawn_blocking`).
/// Returns an empty Vec when the module is gated off, the library is empty, or no
/// row matches — the caller then simply adds no local section. `gated` makes the
/// fetch respect the intelligent-search toggle (main cortinilla); the immersive
/// search passes `false` since it has its own enable.
///
/// Independent of the Qobuz search: callers `tokio::join!` this with
/// `core.search_all`, so an offline / slow Qobuz never blocks the on-device
/// results (and vice-versa).
pub async fn load_cortinilla_local(
    query: &str,
    limit: u64,
    gated: bool,
) -> Vec<qbz_library::LocalTrack> {
    // Gate: the MAIN cortinilla only touches the DB when the intelligent-search
    // module is enabled (`gated = true`). The immersive search is governed by its
    // own "search action" enable instead, so it passes `gated = false`.
    if gated && !crate::search_service::is_enabled() {
        log::info!("[qbz-slint] cortinilla local: gated off (intelligent-search disabled)");
        return Vec::new();
    }
    let q = query.trim().to_string();
    if q.chars().count() < 2 {
        return Vec::new();
    }
    let exclude_network = crate::local_library::exclude_network_folders_now();
    // Plex is part of the user's Local Library (the Artists/Tracks tabs union it),
    // so the cortinilla must include it too. `db.search` only hits `local_tracks`;
    // the Plex cache is a separate bounded set merged here (same shape as the
    // LocalLibrary Tracks tab, see local_library::fetch_tracks_page).
    let plex_enabled = crate::plex_settings::get().enabled;
    let q_log = q.clone();
    let rows: Vec<qbz_library::LocalTrack> = tokio::task::spawn_blocking(move || {
        let mut rows = crate::library_db::with_db(|db| {
            // "default" sort: the cortinilla has no sort control; keep the
            // historical album-grouped order.
            db.search_with_filter_page(q.trim(), 0, limit, true, exclude_network, "default")
        })
        .unwrap_or_default();
        if plex_enabled {
            if let Ok(plex_rows) =
                qbz_plex::plex_cache_search_tracks(q.trim().to_string(), None)
            {
                // Map Plex rows into the LocalTrack shape (source=plex, file_path=
                // rating_key, plex:album: group keys) and PREPEND so Plex content
                // is visible without scrolling past a full local page.
                let mut merged: Vec<qbz_library::LocalTrack> = plex_rows
                    .into_iter()
                    .map(crate::local_library::map_plex_cached_to_local_track)
                    .collect();
                merged.append(&mut rows);
                rows = merged;
            }
        }
        rows
    })
    .await
    .unwrap_or_default();
    log::debug!(
        "[qbz-slint] cortinilla local: query={q_log:?} limit={limit} plex={plex_enabled} -> {} rows",
        rows.len()
    );
    rows
}

/// (Re)assign `flat_index` across a cortinilla payload: top-result = 0, then
/// each section's rows in declaration order, 1..N. Called after the local
/// section is appended too, so indices stay contiguous.
pub fn assign_flat_indices(data: &mut CortinillaData) {
    let mut next = 0usize;
    if let Some(top) = data.top.as_mut() {
        top.flat_index = next;
    }
    // Whether or not there is a top result, section rows start at 1 (index 0 is
    // reserved for the top-result slot the overlay always treats as flat 0).
    next = 1;
    for section in data.sections.iter_mut() {
        for row in section.rows.iter_mut() {
            row.flat_index = next;
            next += 1;
        }
    }
}

// ==================== Load (async, worker thread) ====================

/// Run a combined search and map it to plain `Send` data. The search and
/// the user's followed-artist set are fetched concurrently.
pub async fn load_search<A>(
    runtime: &Arc<AppRuntime<A>>,
    query: &str,
) -> Result<SearchData, String>
where
    A: FrontendAdapter + Send + Sync + 'static,
{
    // Blacklist filtering (featured-aware via qbz-core helpers); skipped when the feature is disabled.
    let blacklist = if crate::artist_blacklist::is_enabled() {
        crate::artist_blacklist::ids_snapshot()
    } else {
        std::collections::HashSet::new()
    };
    // Album axis shares the same enabled gate.
    let album_blacklist = if crate::artist_blacklist::is_enabled() {
        crate::artist_blacklist::album_ids_snapshot()
    } else {
        std::collections::HashSet::new()
    };
    let core = runtime.core();
    let (results, favs) = tokio::join!(
        core.search_all(query, &blacklist, &album_blacklist),
        core.favorite_artist_ids(),
    );
    let results = results.map_err(|e| e.to_string())?;
    // Replace the cache only on a SUCCESSFUL fetch: set_all_artists syncs to
    // the per-user disk store, so wiping it on a failed fetch (empty default)
    // corrupted the follow set across restarts — the Home/ForYou Pinned
    // carousel seeds from it at build time and showed every artist as
    // not-followed. A failed fetch still maps this page with an empty set
    // (transient), but must not touch the persisted cache.
    let favs_ok = favs.is_ok();
    let favs = favs.unwrap_or_default();
    if favs_ok {
        // Seed the in-memory artist fav cache so the follow toggle has current state.
        crate::fav_cache::set_all_artists(favs.clone());
    }
    Ok(map_search_all(query, results, &favs))
}

/// Run a combined search for the live cortinilla, store it in the per-user
/// cache, and map it to the dropdown payload. Reuses the same blacklist +
/// `search_all` shape as [`load_search`]. The learned top-result for the query
/// is folded in by `map_search_all_to_cortinilla`.
///
/// Returns the mapped dropdown payload AND the raw `LocalTrack` rows that backed
/// the on-device section, so the caller can snapshot them for click routing
/// (the click router plays a local row through `playback::play_local_tracks`,
/// which needs the concrete `LocalTrack`, not just its id).
pub async fn load_cortinilla<A>(
    runtime: &Arc<AppRuntime<A>>,
    query: &str,
    expand_local: bool,
) -> Result<(CortinillaData, Vec<qbz_library::LocalTrack>), String>
where
    A: FrontendAdapter + Send + Sync + 'static,
{
    let blacklist = if crate::artist_blacklist::is_enabled() {
        crate::artist_blacklist::ids_snapshot()
    } else {
        std::collections::HashSet::new()
    };
    // Album axis shares the same enabled gate.
    let album_blacklist = if crate::artist_blacklist::is_enabled() {
        crate::artist_blacklist::album_ids_snapshot()
    } else {
        std::collections::HashSet::new()
    };
    // Offline / not-signed-in → the dropdown is local-only, so widen the local
    // section caps (and the raw fetch that feeds the derived album/artist groups).
    let caps = LocalCaps::for_session(expand_local);
    let core = runtime.core();
    // Fire the Qobuz search and the local-library search CONCURRENTLY. The local
    // query is independent — if Qobuz is slow/offline the on-device rows still
    // fill in (a Qobuz error falls into the local-only branch below instead of
    // discarding everything; the local rows are already resolved by then).
    let (results, local_rows) = tokio::join!(
        core.search_all(query, &blacklist, &album_blacklist),
        load_cortinilla_local(query, caps.fetch_limit(), true)
    );
    let mut data = match results {
        Ok(results) => {
            // NOTE: the live page is NOT persisted into the result cache
            // (CAPA A). Nothing has ever read it — `search_service::cached`
            // has zero non-test callers, and `git log -S` shows the read side
            // was never wired in any commit of this repo's history. The write
            // cost a full serialize + `fs::write` of a multi-megabyte artist
            // store on this thread, per keystroke, to feed a file nobody
            // opens. CAPA B (the learned ranking) below is the half that does
            // reach the user.
            let top = crate::search_service::top_for_query(query);
            map_search_all_to_cortinilla(query, &results, top)
        }
        Err(e) => {
            // Qobuz failed (offline / API error). The on-device rows resolved
            // independently, so still build a dropdown from JUST the local
            // section rather than dropping everything. An empty local set then
            // yields an empty payload (the overlay shows only "Search for …").
            log::debug!("[qbz-slint] cortinilla: qobuz search failed ({e}); local-only");
            CortinillaData {
                query: query.to_string(),
                top: None,
                sections: Vec::new(),
            }
        }
    };
    // Append the local "on this device" sections LAST (after every Qobuz
    // category) and re-run flat-index assignment so the local rows get
    // contiguous indices.
    append_local_sections(&mut data, &local_rows, caps);
    Ok((data, local_rows))
}

/// Run a combined search for the in-immersive dropdown and map it to the
/// immersive payload (Albums/Artists/Playlists only — no local section, no
/// top-result hero). Reuses the same blacklist snapshot + `search_all` shape as
/// [`load_cortinilla`], but does NOT query the local library (immersive has no
/// on-device section) and does NOT persist to the search cache / learn a top
/// result (the immersive dropdown is playback-only, so the ranking-feedback
/// surface stays the main cortinilla's).
pub async fn load_immersive_search<A>(
    runtime: &Arc<AppRuntime<A>>,
    query: &str,
    expand_local: bool,
) -> Result<CortinillaData, String>
where
    A: FrontendAdapter + Send + Sync + 'static,
{
    let blacklist = if crate::artist_blacklist::is_enabled() {
        crate::artist_blacklist::ids_snapshot()
    } else {
        std::collections::HashSet::new()
    };
    // Album axis shares the same enabled gate.
    let album_blacklist = if crate::artist_blacklist::is_enabled() {
        crate::artist_blacklist::album_ids_snapshot()
    } else {
        std::collections::HashSet::new()
    };
    let caps = LocalCaps::for_session(expand_local);
    let core = runtime.core();
    // Qobuz catalog + local albums CONCURRENTLY. Local is ungated (immersive
    // search has its own "search action" enable, independent of the main
    // cortinilla's intelligent-search toggle).
    let (results, local_rows) = tokio::join!(
        core.search_all(query, &blacklist, &album_blacklist),
        load_cortinilla_local(query, caps.fetch_limit(), false)
    );
    // A Qobuz error (offline / not signed in) still yields a local-only dropdown
    // rather than discarding everything — the local albums are already resolved.
    let mut data = match results {
        Ok(results) => map_search_all_to_immersive(query, &results),
        Err(e) => {
            log::debug!("[qbz-slint] immersive search: qobuz failed ({e}); local-only");
            CortinillaData {
                query: query.to_string(),
                top: None,
                sections: Vec::new(),
            }
        }
    };
    // Immersive shows albums ONLY; selecting one queues it per the action.
    append_immersive_local_albums(&mut data, &local_rows, caps.albums);
    Ok(data)
}

// ==================== Apply (Slint event loop) ====================

fn album_item(row: AlbumRow) -> AlbumCardItem {
    AlbumCardItem {
        plays: 0,
        // Favorite heart state from the login-seeded cache (kept live by
        // main::set_album_row_favorite when a favorite toggles anywhere).
        is_favorite: crate::fav_cache::is_album_favorite(&row.id),
        // Pin badge state from the per-user pinned store (kept live by
        // main::set_album_row_pinned when a pin toggles anywhere).
        is_pinned: crate::pinned::is_pinned("album", &row.id),
        id: row.id.into(),
        title: row.title.into(),
        artist: row.artist.into(),
        artist_id: row.artist_id.into(),
        genre: row.genre.into(),
        year: row.year.into(),
        quality_tier: row.quality_tier.into(),
        quality_label: row.quality_label.into(),
        ribbon: Default::default(),
        ribbon_kind: Default::default(),
        artwork_url: row.artwork_url.into(),
        artwork: slint::Image::default(),
        ..Default::default()
    }
}

fn track_item(row: TrackRowData) -> TrackItem {
    let is_favorite = crate::fav_cache::is_favorite(&row.id);
    let is_cached = crate::offline_cache::is_cached(&row.id);
    TrackItem {
        // Combined search DROPS blacklisted rows at build time (T4 snapshot
        // filter), so a row reaching here is never blacklisted (no greyout).
        is_blacklisted: false,
        id: row.id.into(),
        number: "".into(),
        title: row.title.into(),
        artist: row.artist.into(),
        album: "".into(),
        duration: row.duration.into(),
        quality_tier: row.quality_tier.into(),
        quality_detail: row.quality_detail.into(),
        explicit: row.explicit,
        selected: false,
        artwork_url: row.artwork_url.into(),
        artwork: slint::Image::default(),
        is_favorite,
        artist_id: row.artist_id.into(),
        album_id: row.album_id.into(),
        removing: false,
        cache_status: if is_cached { 3 } else { 0 },
        cache_progress: 0.0,
        source: "qobuz".into(),
        unlocking: false,
        // Disc grouping is album-detail only; flat lists carry none.
        disc_header_number: 0,
        // Work grouping is album-detail only too.
        work_header: "".into(),
        work_composer_name: "".into(),
        work_composer_id: "".into(),
    }
}

fn artist_item(row: ArtistRow) -> SlimItem {
    SlimItem {
        // Pin badge state from the per-user pinned store (kept live by
        // main::set_artist_row_pinned when a pin toggles anywhere). First:
        // it must borrow `row.id` before the `id:` initializer moves it.
        is_pinned: crate::pinned::is_pinned("artist", &row.id),
        id: row.id.into(),
        title: row.name.into(),
        subtitle: row.subtitle.into(),
        rank: Default::default(),
        artwork_url: row.artwork_url.into(),
        artwork: slint::Image::default(),
        following: row.following,
    }
}

pub(crate) fn playlist_item(row: PlaylistRow) -> SearchPlaylistItem {
    let url = |i: usize| -> slint::SharedString {
        row.cover_urls.get(i).cloned().unwrap_or_default().into()
    };
    SearchPlaylistItem {
        // Pin badge state from the per-user pinned store (kept live by
        // main::set_playlist_row_pinned when a pin toggles anywhere). First:
        // it must borrow `row.id` before the `id:` initializer moves it.
        is_pinned: crate::pinned::is_pinned("playlist", &row.id),
        id: row.id.into(),
        title: row.title.into(),
        subtitle: row.subtitle.into(),
        cover_count: row.cover_urls.len().min(4) as i32,
        url1: url(0),
        url2: url(1),
        url3: url(2),
        url4: url(3),
        cover1: slint::Image::default(),
        cover2: slint::Image::default(),
        cover3: slint::Image::default(),
        cover4: slint::Image::default(),
        // Search playlist results carry no category subtag, and a transparent
        // dominant-colour is the sentinel for "no letterbox" — the collage
        // keeps the legacy cover-fit (contain + dominant colour is Discover-
        // only).
        category: "".into(),
        dominant_color: slint::Color::from_argb_u8(0, 0, 0, 0),
        is_owned: row.is_owned,
        is_following: row.is_following,
        is_copied: row.is_copied,
    }
}

/// Apply search results to the `SearchState` global. Runs on the Slint
/// event loop.
pub fn apply_search(window: &AppWindow, data: SearchData) {
    let state = window.global::<SearchState>();
    state.set_query(data.query.into());

    let albums: Vec<AlbumCardItem> = data.albums.into_iter().map(album_item).collect();
    let tracks: Vec<TrackItem> = data.tracks.into_iter().map(track_item).collect();
    let artists: Vec<SlimItem> = data.artists.into_iter().map(artist_item).collect();
    let playlists: Vec<SearchPlaylistItem> =
        data.playlists.into_iter().map(playlist_item).collect();
    // Carousel variant of the artists list — drops the first entry when
    // it equals the most-popular hero, so the All tab does not duplicate
    // the Top result alongside the carousel.
    let mp_id = if let MostPopularRow::Artist(ref mp) = data.most_popular {
        Some(mp.id.clone())
    } else {
        None
    };
    let artists_carousel: Vec<SlimItem> = match (mp_id, artists.first()) {
        (Some(id), Some(first)) if first.id == id.as_str() => artists[1..].to_vec(),
        _ => artists.clone(),
    };

    state.set_albums(ModelRc::new(VecModel::from(albums)));
    state.set_tracks(ModelRc::new(VecModel::from(tracks)));
    state.set_artists(ModelRc::new(VecModel::from(artists)));
    state.set_artists_carousel(ModelRc::new(VecModel::from(artists_carousel)));
    state.set_playlists(ModelRc::new(VecModel::from(playlists)));

    state.set_albums_total(data.albums_total as i32);
    state.set_tracks_total(data.tracks_total as i32);
    state.set_artists_total(data.artists_total as i32);
    state.set_playlists_total(data.playlists_total as i32);

    // Default the hero quality label off; only the track branch sets it.
    state.set_most_popular_quality_label("".into());
    match data.most_popular {
        MostPopularRow::Album(row) => {
            state.set_most_popular_kind("album".into());
            state.set_most_popular_album(album_item(row));
        }
        MostPopularRow::Artist(row) => {
            state.set_most_popular_kind("artist".into());
            state.set_most_popular_artist(artist_item(row));
        }
        MostPopularRow::Track(row) => {
            state.set_most_popular_kind("track".into());
            state.set_most_popular_quality_label(row.quality_label.clone().into());
            state.set_most_popular_track(track_item(row));
        }
        MostPopularRow::None => {
            state.set_most_popular_kind("".into());
        }
    }
}

// ==================== Cortinilla apply + artwork ====================

/// Turn a plain `CortRow` into its Slint item. `artwork` starts empty; the
/// artwork pipeline resolves it in place keyed off `flat_index`.
fn cortinilla_row_item(row: &CortRow) -> CortinillaRowItem {
    CortinillaRowItem {
        kind: row.kind.clone().into(),
        id: row.id.clone().into(),
        source: row.source.clone().into(),
        title: row.title.clone().into(),
        subtitle: row.subtitle.clone().into(),
        artwork_url: row.artwork_url.clone().into(),
        artwork: slint::Image::default(),
        flat_index: row.flat_index as i32,
    }
}

/// Write a cortinilla payload into `SearchState`. Runs on the Slint event loop.
/// Clears `cortinilla-loading`. Does NOT reset `selected-index` here — the live
/// handler resets selection only when the query actually changed, so a late
/// revalidation overwrite keeps the user's current highlight.
pub fn apply_cortinilla(window: &AppWindow, data: CortinillaData) {
    let state = window.global::<SearchState>();
    state.set_cortinilla_query(data.query.clone().into());

    // Top result — an empty CortinillaRow (kind == "") means "no top result".
    match &data.top {
        Some(top) => state.set_top_result(cortinilla_row_item(top)),
        // An all-default row (kind == "", id == "") is the overlay's "no top
        // result" sentinel.
        None => state.set_top_result(CortinillaRowItem::default()),
    }

    let sections: Vec<CortinillaSectionItem> = data
        .sections
        .iter()
        .map(|s| {
            let rows: Vec<CortinillaRowItem> = s.rows.iter().map(cortinilla_row_item).collect();
            CortinillaSectionItem {
                title: s.title.clone().into(),
                kind: s.kind.clone().into(),
                rows: ModelRc::new(VecModel::from(rows)),
                has_more: s.has_more,
            }
        })
        .collect();
    state.set_sections(ModelRc::new(VecModel::from(sections)));
    state.set_cortinilla_loading(false);
}

/// Build artwork download jobs for a cortinilla payload — one per row that
/// carries a URL, keyed by the row's stable `flat_index` (top-result = 0).
pub fn cortinilla_artwork_jobs(data: &CortinillaData) -> Vec<ArtworkJob> {
    let mut jobs = Vec::new();
    if let Some(top) = &data.top {
        jobs.extend(simple_job(
            ArtworkTarget::CortinillaRow {
                flat_index: top.flat_index,
            },
            &top.artwork_url,
        ));
    }
    for section in &data.sections {
        for row in &section.rows {
            jobs.extend(simple_job(
                ArtworkTarget::CortinillaRow {
                    flat_index: row.flat_index,
                },
                &row.artwork_url,
            ));
        }
    }
    jobs
}

/// Write an immersive-search payload into `ImmersiveState`. Runs on the Slint
/// event loop. Builds the `[CortinillaSection]` model from `data.sections`
/// (reusing the shared row/section item-builders) and clears `search-loading`.
/// Does NOT touch `search-selected-index` — the controller owns selection and
/// resets it on every open/refine, so a late revalidation must not clobber it.
/// The immersive payload has no top result, so none is written.
pub fn apply_immersive_search(window: &AppWindow, data: &CortinillaData) {
    let state = window.global::<crate::ImmersiveState>();
    let sections: Vec<CortinillaSectionItem> = data
        .sections
        .iter()
        .map(|s| {
            let rows: Vec<CortinillaRowItem> = s.rows.iter().map(cortinilla_row_item).collect();
            CortinillaSectionItem {
                title: s.title.clone().into(),
                kind: s.kind.clone().into(),
                rows: ModelRc::new(VecModel::from(rows)),
                has_more: s.has_more,
            }
        })
        .collect();
    state.set_search_sections(ModelRc::new(VecModel::from(sections)));
    state.set_search_loading(false);
}

/// Build artwork download jobs for an immersive-search payload — one per row
/// that carries a URL, keyed by the row's stable `flat_index`. Targets
/// `ImmersiveSearchRow` (the immersive global) instead of `CortinillaRow`. The
/// immersive payload has no top result, so only section rows produce jobs.
pub fn immersive_cortinilla_artwork_jobs(data: &CortinillaData) -> Vec<ArtworkJob> {
    let mut jobs = Vec::new();
    for section in &data.sections {
        for row in &section.rows {
            jobs.extend(simple_job(
                ArtworkTarget::ImmersiveSearchRow {
                    flat_index: row.flat_index,
                },
                &row.artwork_url,
            ));
        }
    }
    jobs
}

/// Clear search state and show the loading state (used when starting a new
/// search so the previous results do not flash).
pub fn reset_search(window: &AppWindow) {
    let state = window.global::<SearchState>();
    state.set_albums(ModelRc::new(VecModel::from(Vec::<AlbumCardItem>::new())));
    state.set_tracks(ModelRc::new(VecModel::from(Vec::<TrackItem>::new())));
    state.set_artists(ModelRc::new(VecModel::from(Vec::<SlimItem>::new())));
    state.set_playlists(ModelRc::new(VecModel::from(Vec::<SearchPlaylistItem>::new())));
    state.set_albums_total(0);
    state.set_tracks_total(0);
    state.set_artists_total(0);
    state.set_playlists_total(0);
    state.set_most_popular_kind("".into());
    state.set_most_popular_quality_label("".into());
    state.set_filter_index(0);
    state.set_loading(true);
}

/// Mark an artist as followed in every `SearchState` list it appears in
/// (results list + most-popular hero). Runs on the Slint event loop.
pub fn mark_artist_followed(window: &AppWindow, artist_id: &str, following: bool) {
    // Flip the Follow chip on EVERY visible artist-card surface, so following
    // from any of them updates the others (Search, For You, the Recommendations
    // carousels). "auto"-mode chips hide once following; "toggle"-mode flip.
    let state = window.global::<SearchState>();
    set_slim_following(&state.get_artists(), artist_id, following);
    set_slim_following(&state.get_artists_carousel(), artist_id, following);
    if state.get_most_popular_kind() == "artist" {
        let mut mp = state.get_most_popular_artist();
        if mp.id == artist_id {
            mp.following = following;
            state.set_most_popular_artist(mp);
        }
    }
    let foryou = window.global::<ForYouState>();
    set_slim_following(&foryou.get_top_artists(), artist_id, following);
    set_slim_following(&foryou.get_artists_to_follow(), artist_id, following);
    let reco = window.global::<ExternalRecoState>();
    set_slim_following(&reco.get_rec_artists_common(), artist_id, following);
    set_slim_following(&reco.get_rec_artists_recent(), artist_id, following);
    set_slim_following(&reco.get_top_artists(), artist_id, following);
    // Home "Top artists" carousel + label-page related artists — walked by the
    // pin twin (`set_artist_row_pinned`) but historically missed here.
    set_slim_following(&window.global::<HomeState>().get_top_artists(), artist_id, following);
    set_slim_following(&window.global::<LabelState>().get_artists(), artist_id, following);
    // Scene-discovery grid (ArtistsByLocationView) — SlimItem rows since the
    // ArtistGridCard migration.
    set_slim_following(
        &window.global::<crate::LocationViewState>().get_artists(),
        artist_id,
        following,
    );
    // Pinned mixed carousel (Home / For You) — nested artist SlimItem.
    crate::set_pinned_artist_following(window, artist_id, following);
}

/// Flip `following` on the row matching `artist_id` in a `[SlimItem]` model.
fn set_slim_following(model: &ModelRc<SlimItem>, artist_id: &str, following: bool) {
    if let Some(vm) = model.as_any().downcast_ref::<VecModel<SlimItem>>() {
        for i in 0..vm.row_count() {
            if let Some(mut item) = vm.row_data(i) {
                if item.id == artist_id {
                    item.following = following;
                    vm.set_row_data(i, item);
                }
            }
        }
    }
}

// ==================== Load-more (pagination) ====================

/// Which category a load-more request targets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SearchCategory {
    Albums,
    Tracks,
    Artists,
    Playlists,
}

/// Map a results-tab index to the category whose list it paginates.
/// Tab 0 (All) has no single category.
pub fn category_for_tab(tab: i32) -> Option<SearchCategory> {
    match tab {
        1 => Some(SearchCategory::Albums),
        2 => Some(SearchCategory::Tracks),
        3 => Some(SearchCategory::Artists),
        4 => Some(SearchCategory::Playlists),
        _ => None,
    }
}

/// Map a filter index to the Qobuz `search_type` value. Index 0 maps to
/// `None` (no filter).
pub fn search_type_for_filter(index: i32) -> Option<String> {
    match index {
        1 => Some("MainArtist".into()),
        2 => Some("Performer".into()),
        3 => Some("Composer".into()),
        4 => Some("Label".into()),
        5 => Some("ReleaseName".into()),
        _ => None,
    }
}

/// A page of additional rows fetched by load-more, ready to append.
pub enum MoreRows {
    Albums(Vec<AlbumRow>),
    Tracks(Vec<TrackRowData>),
    Artists(Vec<ArtistRow>),
    Playlists(Vec<PlaylistRow>),
}

/// Load-more page size (matches the Tauri search page size).
const PAGE_SIZE: u32 = 20;

/// Fetch the next page for one category, starting at `offset`.
pub async fn load_more<A>(
    runtime: &Arc<AppRuntime<A>>,
    query: &str,
    category: SearchCategory,
    search_type: Option<String>,
    offset: u32,
) -> Result<MoreRows, String>
where
    A: FrontendAdapter + Send + Sync + 'static,
{
    let core = runtime.core();
    let search_type = search_type.as_deref();
    // Page-2+ was unfiltered before (the core pass-throughs carry no &bl).
    // Post-filter here, closing both the album and the artist leak. Shared
    // enabled gate.
    let (bl, abl) = if crate::artist_blacklist::is_enabled() {
        (
            crate::artist_blacklist::ids_snapshot(),
            crate::artist_blacklist::album_ids_snapshot(),
        )
    } else {
        Default::default()
    };
    match category {
        SearchCategory::Albums => {
            let page = core
                .search_albums(query, PAGE_SIZE, offset, search_type)
                .await
                .map_err(|e| e.to_string())?;
            Ok(MoreRows::Albums(
                page.items
                    .into_iter()
                    .filter(|a| !qbz_core::core::album_blacklisted(a, &bl, &abl))
                    .map(map_album)
                    .collect(),
            ))
        }
        SearchCategory::Tracks => {
            let page = core
                .search_tracks(query, PAGE_SIZE, offset, search_type)
                .await
                .map_err(|e| e.to_string())?;
            Ok(MoreRows::Tracks(
                page.items
                    .into_iter()
                    .filter(|t| !qbz_core::core::track_blacklisted(t, &bl, &abl))
                    .map(map_track)
                    .collect(),
            ))
        }
        SearchCategory::Artists => {
            let (page, favs) = tokio::join!(
                core.search_artists(query, PAGE_SIZE, offset, search_type),
                core.favorite_artist_ids(),
            );
            let page = page.map_err(|e| e.to_string())?;
            let favs = favs.unwrap_or_default();
            Ok(MoreRows::Artists(
                page.items
                    .iter()
                    .filter(|a| !bl.contains(&a.id))
                    .map(|a| map_artist(a, favs.contains(&a.id)))
                    .collect(),
            ))
        }
        SearchCategory::Playlists => {
            let page = core
                .search_playlists(query, PAGE_SIZE, offset)
                .await
                .map_err(|e| e.to_string())?;
            Ok(MoreRows::Playlists(
                page.items.into_iter().map(map_playlist).collect(),
            ))
        }
    }
}

/// Append fetched rows to the matching `SearchState` list. Pushes onto the
/// existing `VecModel` so already-loaded rows (and any resolved artwork)
/// are untouched. Runs on the Slint event loop.
pub fn append_results(window: &AppWindow, more: MoreRows) {
    let state = window.global::<SearchState>();
    match more {
        MoreRows::Albums(rows) => {
            if let Some(vm) = state
                .get_albums()
                .as_any()
                .downcast_ref::<VecModel<AlbumCardItem>>()
            {
                for row in rows {
                    vm.push(album_item(row));
                }
            }
        }
        MoreRows::Tracks(rows) => {
            if let Some(vm) = state
                .get_tracks()
                .as_any()
                .downcast_ref::<VecModel<TrackItem>>()
            {
                for row in rows {
                    vm.push(track_item(row));
                }
            }
        }
        MoreRows::Artists(rows) => {
            if let Some(vm) = state
                .get_artists()
                .as_any()
                .downcast_ref::<VecModel<SlimItem>>()
            {
                for row in rows {
                    vm.push(artist_item(row));
                }
            }
        }
        MoreRows::Playlists(rows) => {
            if let Some(vm) = state
                .get_playlists()
                .as_any()
                .downcast_ref::<VecModel<SearchPlaylistItem>>()
            {
                for row in rows {
                    vm.push(playlist_item(row));
                }
            }
        }
    }
}

/// Replace one category's `SearchState` list wholesale — used when the
/// searchType filter changes and the category is re-queried from offset 0.
pub fn replace_category(window: &AppWindow, more: MoreRows) {
    let state = window.global::<SearchState>();
    match more {
        MoreRows::Albums(rows) => {
            let items: Vec<AlbumCardItem> = rows.into_iter().map(album_item).collect();
            state.set_albums(ModelRc::new(VecModel::from(items)));
        }
        MoreRows::Tracks(rows) => {
            let items: Vec<TrackItem> = rows.into_iter().map(track_item).collect();
            state.set_tracks(ModelRc::new(VecModel::from(items)));
        }
        MoreRows::Artists(rows) => {
            // Rebuild both lists: the Artists tab keeps every result; the
            // All-tab carousel drops the duplicate next to the Most-popular
            // hero.
            let items: Vec<SlimItem> = rows.into_iter().map(artist_item).collect();
            let mp_id = if state.get_most_popular_kind().as_str() == "artist" {
                Some(state.get_most_popular_artist().id)
            } else {
                None
            };
            let carousel: Vec<SlimItem> = match (mp_id, items.first()) {
                (Some(id), Some(first)) if first.id == id.as_str() => items[1..].to_vec(),
                _ => items.clone(),
            };
            state.set_artists(ModelRc::new(VecModel::from(items)));
            state.set_artists_carousel(ModelRc::new(VecModel::from(carousel)));
        }
        MoreRows::Playlists(rows) => {
            let items: Vec<SearchPlaylistItem> = rows.into_iter().map(playlist_item).collect();
            state.set_playlists(ModelRc::new(VecModel::from(items)));
        }
    }
}

// ==================== Artwork jobs ====================

/// Cover-download jobs for an album/track/artist row at `idx`.
fn simple_job(target: ArtworkTarget, url: &str) -> Option<ArtworkJob> {
    (!url.is_empty()).then(|| ArtworkJob {
        target,
        url: url.to_string(),
    })
}

/// Playlist collage jobs — one per cover URL the row carries.
fn playlist_jobs(idx: usize, urls: &[String], jobs: &mut Vec<ArtworkJob>) {
    for (slot, url) in urls.iter().enumerate().take(4) {
        if !url.is_empty() {
            jobs.push(ArtworkJob {
                target: ArtworkTarget::SearchPlaylistCover { idx, slot },
                url: url.clone(),
            });
        }
    }
}

/// Build artwork download jobs for a freshly applied `SearchData`.
pub fn artwork_jobs(data: &SearchData) -> Vec<ArtworkJob> {
    let mut jobs = Vec::new();
    for (idx, row) in data.albums.iter().enumerate() {
        jobs.extend(simple_job(ArtworkTarget::SearchAlbum { idx }, &row.artwork_url));
    }
    for (idx, row) in data.tracks.iter().enumerate() {
        jobs.extend(simple_job(ArtworkTarget::SearchTrack { idx }, &row.artwork_url));
    }
    for (idx, row) in data.artists.iter().enumerate() {
        jobs.extend(simple_job(ArtworkTarget::SearchArtist { idx }, &row.artwork_url));
    }
    for (idx, row) in data.playlists.iter().enumerate() {
        playlist_jobs(idx, &row.cover_urls, &mut jobs);
    }
    let mp_url = match &data.most_popular {
        MostPopularRow::Album(r) => r.artwork_url.as_str(),
        MostPopularRow::Artist(r) => r.artwork_url.as_str(),
        MostPopularRow::Track(r) => r.artwork_url.as_str(),
        MostPopularRow::None => "",
    };
    jobs.extend(simple_job(ArtworkTarget::SearchMostPopular, mp_url));
    jobs
}

/// Build artwork jobs for a load-more page, targeting the rows that were
/// just appended (`start` is the index of the first appended row).
pub fn artwork_jobs_for_more(more: &MoreRows, start: usize) -> Vec<ArtworkJob> {
    let mut jobs = Vec::new();
    match more {
        MoreRows::Albums(rows) => {
            for (i, row) in rows.iter().enumerate() {
                jobs.extend(simple_job(
                    ArtworkTarget::SearchAlbum { idx: start + i },
                    &row.artwork_url,
                ));
            }
        }
        MoreRows::Tracks(rows) => {
            for (i, row) in rows.iter().enumerate() {
                jobs.extend(simple_job(
                    ArtworkTarget::SearchTrack { idx: start + i },
                    &row.artwork_url,
                ));
            }
        }
        MoreRows::Artists(rows) => {
            for (i, row) in rows.iter().enumerate() {
                jobs.extend(simple_job(
                    ArtworkTarget::SearchArtist { idx: start + i },
                    &row.artwork_url,
                ));
            }
        }
        MoreRows::Playlists(rows) => {
            for (i, row) in rows.iter().enumerate() {
                playlist_jobs(start + i, &row.cover_urls, &mut jobs);
            }
        }
    }
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_for_tab_maps_per_type_tabs() {
        assert_eq!(category_for_tab(0), None);
        assert_eq!(category_for_tab(1), Some(SearchCategory::Albums));
        assert_eq!(category_for_tab(2), Some(SearchCategory::Tracks));
        assert_eq!(category_for_tab(3), Some(SearchCategory::Artists));
        assert_eq!(category_for_tab(4), Some(SearchCategory::Playlists));
        assert_eq!(category_for_tab(9), None);
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
    fn mmss_pads_seconds() {
        assert_eq!(mmss(5), "0:05");
        assert_eq!(mmss(65), "1:05");
        assert_eq!(mmss(225), "3:45");
    }

    #[test]
    fn tier_classifies_bit_depth() {
        assert_eq!(tier(Some(24)), "hires");
        assert_eq!(tier(Some(16)), "cd");
        assert_eq!(tier(None), "");
    }

    #[test]
    fn quality_label_formats_known_quality() {
        assert_eq!(quality_label(Some(24), Some(96.0)), "Hi-Res 24-bit / 96 kHz");
        assert_eq!(quality_label(Some(16), Some(44.1)), "CD 16-bit / 44.1 kHz");
        assert_eq!(quality_label(None, None), "");
    }

    #[test]
    fn map_artist_builds_album_count_subtitle() {
        let artist = Artist {
            id: 7,
            name: "Metallica".into(),
            image: None,
            albums_count: Some(12),
            biography: None,
            albums: None,
            tracks_appears_on: None,
            playlists: None,
        };
        let row = map_artist(&artist, true);
        assert_eq!(row.id, "7");
        assert_eq!(row.name, "Metallica");
        assert_eq!(row.subtitle, "12 albums");
        assert!(row.following);
    }
}
