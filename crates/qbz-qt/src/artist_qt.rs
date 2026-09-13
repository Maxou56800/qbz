//! Artist detail data layer — Slint-free port of `crates/qbz/src/artist.rs`
//! (`load_artist`/`map_artist`, `load_release_page`, the release-section
//! bucketing in the official order). Publishes ONE JSON document.
//!
//! The page publishes PROGRESSIVELY: `load_artist_view` returns the Qobuz
//! document immediately (main.rs sets it on the bridge), then a background
//! task re-publishes the SAME document enriched with the Magazine stories and
//! the MusicBrainz network-sidebar sections as each one resolves. The stashed
//! copy is generation-guarded so a late reply for a previous artist is
//! dropped instead of overwriting the current page.
//!
//! MusicBrainz is OPT-IN-RESPECTING: `network.mbAvailable` is seeded from
//! `core.musicbrainz_is_enabled()` BEFORE anything is fetched — when the user
//! has MB off, no resolve/metadata/relationship/discovery request is made at
//! all and the Origin / Relationships / You-may-also-like sections are simply
//! absent from the sidebar (never an error, never an empty frame).
//!
//! Known delta: the "In library" tab has album/track content but not the
//! reference's playlist sub-list.
//! - `is_following` seeds from `library_qt::is_favorite` — the favourite-id
//!   cache first (the reference's `fav_cache`, now ported as `fav_cache_qt`),
//!   the phase-5 library feed second. It used to read the feed ALONE, which is
//!   empty until the Library view is opened: the heart drew un-followed on
//!   artists the user follows, and the toggle then re-ADDED the follow.
//! - Discovery runs with EMPTY `dismissed_per_tag` / `known_artists`
//!   callbacks: the Slint app feeds those from `discovery_dismiss` +
//!   `play_history` + `reco`; this path still uses empty callbacks. That is the
//!   same default a first-run Slint profile lands on (no exclusion applied),
//!   not a fabricated one.
//! - The blacklist banner, Artist Scene/location navigation, collection
//!   builder and jump-scroll tracking are live in `ArtistView.qml`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cxx_qt_lib::QString;
use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use qbz_models::{
    ArtistStoryItem, PageArtistRelease, PageArtistResponse, PageArtistTrack, QueueTrack,
};
use serde::Serialize;
use serde_json::json;

use crate::album_qt::{truncate_words, AlbumCardData, TrackRow};
use crate::home_qt;
use crate::library_qt::FeedItem;

pub const RELEASE_PAGE_SIZE: u32 = 20;

/// Official on-screen order of release buckets (artist.rs
/// RELEASE_SECTION_ORDER; titles are the msgids translated at build time).
const RELEASE_SECTION_ORDER: &[(&str, &str)] = &[
    ("album", "Albums"),
    ("epSingle", "EPs & Singles"),
    ("ep", "EPs & Singles"),
    ("single", "EPs & Singles"),
    ("live", "Live"),
    ("compilation", "Compilations"),
    ("download", "Purchase Only"),
    ("composer", "Composer"),
    ("other", "Other"),
    ("awardedRelease", "Critics' Picks"),
    ("next", "Upcoming"),
];

/// Title-case a raw release_type key — the fallback for a bucket the table
/// above does not know (`qbz/src/artist.rs:117-123` `title_case`).
fn title_case(key: &str) -> String {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Display title for one release_type — the discography page's header
/// (`qbz/src/artist.rs:107-114` `release_type_title`).
///
/// Same table, same fallback, as the per-section headers `map_artist` builds
/// below: the page the user lands on after "See discography" must be titled
/// exactly like the section they clicked from.
///
/// Translated HERE rather than in QML because it travels inside a JSON
/// document — which is also why `artist_releases_qt::republish` exists.
pub(crate) fn release_type_title(release_type: &str) -> String {
    RELEASE_SECTION_ORDER
        .iter()
        .find(|(rt, _)| *rt == release_type)
        .map(|(_, title)| qbz_i18n::t(title))
        .unwrap_or_else(|| title_case(release_type))
}

#[derive(Clone, Serialize)]
pub struct ArtistReleaseSection {
    #[serde(rename = "releaseType")]
    pub release_type: String,
    pub title: String,
    #[serde(rename = "hasMore")]
    pub has_more: bool,
    pub cards: Vec<AlbumCardData>,
    /// The bucket's persisted sort (`default` | `newest` | `oldest` |
    /// `title-asc` | `title-desc`) — the Slint's `ArtistReleaseSection.sort-by`.
    ///
    /// It travels WITH the data because that is what makes the picker come
    /// back showing the user's choice: `ReleaseGrid.slint:34-39` maps this
    /// string back to a `current-index` for its QbzSelect, and the QML port
    /// does the same (`ArtistView.qml`, the ReleaseSection header). The value
    /// is stamped here at page build from `artist_prefs::read_all()`, so the
    /// FIRST paint is already in the right order — the cards below are sorted
    /// before this struct exists (`artist.rs:786-812`: "Apply the persisted
    /// per-bucket sort up front so the first paint already honors the user's
    /// choice").
    #[serde(rename = "sortBy")]
    pub sort_by: String,
}

#[derive(Clone, Serialize)]
pub struct ArtistLabel {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Serialize)]
pub struct ArtistSimilar {
    pub id: String,
    pub name: String,
}

/// One row of the artist page's "Playlists" rail. The shape is
/// `cards/PlaylistCard.qml`'s item contract — that card is what the reference
/// mounts here too (`ArtistPageView.slint:985-995` -> `PlaylistCarousel` ->
/// `discover/PlaylistCard.slint`), so this row publishes the state the card
/// draws instead of leaving it to guess.
#[derive(Clone, Serialize)]
pub struct ArtistPlaylist {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    #[serde(rename = "artUrl")]
    pub art_url: String,
    /// Pin badge state, from the per-user store. Without it the glyph reads
    /// "unpinned" for a playlist that IS pinned and the first click un-pins it
    /// (the exact defect already fixed for this view's album grids).
    #[serde(rename = "isPinned")]
    pub is_pinned: bool,
    /// The library heart (the qbz-local library.db flag).
    #[serde(rename = "isFavorite")]
    pub is_favorite: bool,
    /// The overlay tri-state, same reasoning as `home_qt::map_playlist`: the
    /// Slint reference hard-writes both false (`qbz/src/artist.rs:901-903`)
    /// because its `PlaylistSlim` carries no owner id — `PageArtistPlaylist`
    /// DOES, so this port draws the state the reference guesses at. Strictly a
    /// superset: an editorial playlist's owner is Qobuz, so it still resolves
    /// to the foreign follow arm. Both sets are empty until the first
    /// `get_user_playlists` lands, i.e. a cold page may draw `user-plus` for a
    /// playlist the user owns, exactly as the Home rails do.
    #[serde(rename = "playlistOwned")]
    pub playlist_owned: bool,
    #[serde(rename = "playlistFollowing")]
    pub playlist_following: bool,
    /// True when `artUrl` is the playlist's OWN Qobuz graphic. It always is on
    /// this page — the payload carries `images.rectangle` and nothing else —
    /// and the flag makes the card render it CONTAIN deterministically instead
    /// of measuring the ratio. Those banners are 2.11:1 (800x380) and cropping
    /// them into the 200px square is what the owner reported as "gigantic".
    /// The serde name follows `library_qt.rs:110-114`, the only other producer.
    #[serde(rename = "playlistOwnImage")]
    pub playlist_own_image: bool,
}

/// Magazine story teaser (Qobuz editorial — `/artist/getStory`). Not an
/// integration: it is the same Qobuz session the rest of the page uses.
#[derive(Clone, Default, Serialize)]
pub struct StoryRow {
    pub title: String,
    pub author: String,
    pub excerpt: String,
    pub url: String,
    #[serde(rename = "artUrl")]
    pub art_url: String,
}

/// MusicBrainz Origin block (artist.rs `MbOrigin`). `hasData` is the gate the
/// sidebar uses so an MB-matched artist with no life-span/location renders
/// nothing at all instead of an empty heading.
///
/// The `location*` / `seed*` / `artistName` fields below exist so the Artist
/// Scene can be opened WITHOUT a second MusicBrainz round-trip: `map_origin`
/// already receives the whole `ArtistMetadata`, and until 2026-08-14 it threw
/// all of this away and kept only `location_display`. Everything
/// `discover_artists_by_location` needs is therefore carried on the artist
/// document itself. (The reference solves the same problem with a process-wide
/// `static LOCATION_PARAMS` mutex, `crates/qbz/src/artist.rs:1509-1543`; a
/// document field is the Qt-shaped answer and cannot go stale against the
/// artist being displayed.)
#[derive(Clone, Default, Serialize)]
pub struct MbOriginJson {
    #[serde(rename = "isPerson")]
    pub is_person: bool,
    #[serde(rename = "beginDate")]
    pub begin_date: String,
    #[serde(rename = "endDate")]
    pub end_date: String,
    #[serde(rename = "locationDisplay")]
    pub location_display: String,
    #[serde(rename = "hasData")]
    pub has_data: bool,

    // ---- Artist Scene payload ------------------------------------------
    /// MusicBrainz area id, "" when absent. `area_id` on the discovery call.
    #[serde(rename = "locationAreaId")]
    pub location_area_id: String,
    /// City, "" when absent. The discovery call prefers this over the display
    /// name for `area_name` (ArtistsByLocationView.svelte:364).
    #[serde(rename = "locationCity")]
    pub location_city: String,
    /// Country NAME, "" when absent. Also the hero title fallback.
    #[serde(rename = "locationCountry")]
    pub location_country: String,
    /// ISO country code, "" when absent. This is what selects the flag, and
    /// without it the hero simply has no flag element.
    #[serde(rename = "locationCountryCode")]
    pub location_country_code: String,
    /// "city" | "state" | "country" | "" — feeds `locationClickable` only.
    #[serde(rename = "locationPrecision")]
    pub location_precision: String,
    /// Affinity seeds: the discovery query's genres, and the hero subtitle's
    /// fallback when the backend returns no `genre_summary`.
    #[serde(rename = "seedGenres")]
    pub seed_genres: Vec<String>,
    #[serde(rename = "seedTags")]
    pub seed_tags: Vec<String>,
    /// The MUSICBRAINZ name, not the Qobuz one — it is what the scene hero's
    /// "Based on {artist}" prints (ArtistDetailView.svelte:2915).
    #[serde(rename = "artistName")]
    pub artist_name: String,
    /// Whether the location text is a link. Computed HERE so QML never
    /// re-derives it: Tauri's guard is written without parentheses
    /// (`ArtistDetailView.svelte:2904`) and JS `&&` binds tighter than `||`,
    /// so it reads `(handler && precision != 'country') || city`. The second
    /// arm forgets the handler check and is harmless there only because the
    /// call is optional-chained. This is the INTENDED rule:
    ///
    ///     clickable = has_location && (precision != Country || city.is_some())
    ///
    /// A country-only location has nothing to drill into. The reference
    /// reached the same rule independently (`crates/qbz/src/artist.rs:1561`).
    ///
    /// NOTE: this is necessary but NOT sufficient to enable either door — both
    /// also require a non-empty `mbid` and a live MusicBrainz-enabled check.
    /// `mb_available` is NOT that check; see its own doc below.
    #[serde(rename = "locationClickable")]
    pub location_clickable: bool,
}

/// One Members / Member-Of / Collaborators row (artist.rs `MbRelationshipRow`).
#[derive(Clone, Default, Serialize)]
pub struct MbRelationshipJson {
    pub mbid: String,
    pub name: String,
    pub role: String,
    pub tooltip: String,
}

#[derive(Clone, Default, Serialize)]
pub struct MbRelationshipsJson {
    pub members: Vec<MbRelationshipJson>,
    pub groups: Vec<MbRelationshipJson>,
    pub collaborators: Vec<MbRelationshipJson>,
    #[serde(rename = "hasData")]
    pub has_data: bool,
}

/// One "You may also like" candidate. `qobuzId` is "" when the MB candidate
/// never validated against Qobuz — those rows are informational only.
#[derive(Clone, Default, Serialize)]
pub struct MbDiscoveryJson {
    pub mbid: String,
    pub name: String,
    #[serde(rename = "qobuzId")]
    pub qobuz_id: String,
}

/// Everything the Network sidebar renders beyond LABELS / SIMILAR ARTISTS
/// (which ride the Qobuz artist page). Mirrors Slint's `NetworkSidebarState`.
#[derive(Clone, Default, Serialize)]
pub struct ArtistNetwork {
    /// The user's MusicBrainz OPT-IN, and ONLY that. False = every MB-driven
    /// section is ABSENT (opt-in rule: no error, no frame).
    ///
    /// ⚠ This doc used to claim "MusicBrainz is enabled AND this artist
    /// resolved to an MBID". It never did that: it is assigned from
    /// `musicbrainz_is_enabled()` alone (see `data.network.mb_available`
    /// below), seeded SYNCHRONOUSLY with the first artist frame, while `mbid`
    /// stays "" until the async `publish_network` lands. Anything that needs
    /// "this artist actually has an MBID" must test `mbid` itself — gating an
    /// affordance on this flag makes it live from frame one with no id to
    /// hand it, which is exactly how the Artist Scene doors would break.
    #[serde(rename = "mbAvailable")]
    pub mb_available: bool,
    pub mbid: String,
    #[serde(rename = "originLoading")]
    pub origin_loading: bool,
    pub origin: MbOriginJson,
    #[serde(rename = "relationshipsLoading")]
    pub relationships_loading: bool,
    pub relationships: MbRelationshipsJson,
    #[serde(rename = "discoveryLoading")]
    pub discovery_loading: bool,
    #[serde(rename = "discoveryTag")]
    pub discovery_tag: String,
    pub discovery: Vec<MbDiscoveryJson>,
}

#[derive(Default, Serialize)]
pub struct ArtistViewData {
    pub id: String,
    pub name: String,
    pub bio: String,
    #[serde(rename = "bioShort")]
    pub bio_short: String,
    #[serde(rename = "bioTruncated")]
    pub bio_truncated: bool,
    /// Biography attribution ("TiVo" etc); "" when Qobuz sends none.
    #[serde(rename = "bioSource")]
    pub bio_source: String,
    #[serde(rename = "artUrl")]
    pub artwork_url: String,
    /// A user-picked portrait is registered for this artist. Flips the
    /// portrait menu between "Add image" and "Change image"+"Remove image"
    /// (`ArtistPageView.slint:307-330`).
    #[serde(rename = "hasCustomImage")]
    pub has_custom_image: bool,
    /// Absolute path of that portrait ("" when none). The view prefers it over
    /// the url-keyed pipeline image, and it is also what the header tint and
    /// the atmosphere are derived from — so the flag is never decorative.
    #[serde(rename = "customImagePath")]
    pub custom_image_path: String,
    /// The same file as an ESCAPED `file://` URI, which is what QML must bind.
    /// `"file://" + path` is not equivalent: a portrait picked out of a folder
    /// holding `%`, `#` or `?` renders blank, because QUrl eats those. The raw
    /// path stays published beside it because the Rust side needs to open it.
    #[serde(rename = "customImageUrl")]
    pub custom_image_url: String,
    #[serde(rename = "isFollowing")]
    pub is_following: bool,
    #[serde(rename = "isPinned")]
    pub is_pinned: bool,
    /// Blacklisted state at build time, seeded from the per-user artist
    /// blacklist (1:1 with `qbz/src/main.rs:2653-2659`, which seeds it beside
    /// the pin state before the view flips). `ArtistView.qml` folds it through
    /// `toggleState("artistBlacklist", ...)` to choose between "Blacklist
    /// artist" and "Show artist" and to show the hidden banner. Reads false
    /// when the feature is disabled — the reference accepts that here too.
    #[serde(rename = "isBlacklisted")]
    pub is_blacklisted: bool,
    #[serde(rename = "libraryCount")]
    pub library_count: i64,
    /// The user's OWN rows for this artist — favourites ∪ purchases, one row
    /// per entity (`library_qt::artist_library_items`, contract §3). Published
    /// INSIDE this document rather than read off `QbzLibrary.libraryJson` by
    /// the view: the view used to `JSON.parse` the whole ~10k-row feed per
    /// binding evaluation, and this is also what lets a heart change
    /// (`apply_favorite_change`) and a late feed load (`refresh_library_items`)
    /// repaint the tab through `publish_patch`, the page's one transport.
    #[serde(rename = "libraryItems")]
    pub library_items: Vec<FeedItem>,
    /// Sort of the "In library" Albums section — the same five keys the
    /// release buckets use, persisted by `artist_prefs` under the pseudo
    /// release type `library` (`QbzArtist.setSectionSort("library", …)`).
    /// The rows are sorted in QML (they live there already); only the seed
    /// travels here.
    #[serde(rename = "librarySort")]
    pub library_sort: String,
    #[serde(rename = "topTracks")]
    pub top_tracks: Vec<TrackRow>,
    #[serde(rename = "appearsOn")]
    pub appears_on: Vec<TrackRow>,
    #[serde(rename = "lastRelease")]
    pub last_release: Option<AlbumCardData>,
    #[serde(rename = "releaseSections")]
    pub release_sections: Vec<ArtistReleaseSection>,
    pub labels: Vec<ArtistLabel>,
    #[serde(rename = "similarArtists")]
    pub similar_artists: Vec<ArtistSimilar>,
    pub playlists: Vec<ArtistPlaylist>,
    /// Magazine tab. Empty + `storiesLoading` false = "No stories".
    pub stories: Vec<StoryRow>,
    #[serde(rename = "storiesLoading")]
    pub stories_loading: bool,
    /// MusicBrainz-driven Network sidebar sections (filled progressively).
    pub network: ArtistNetwork,
    // ---- Header atmosphere (controls/HeaderGradient.qml) ------------------
    /// Artwork-derived header tint, "#rrggbb" ("" until the portrait
    /// resolves). Same field, same meaning, same producer as the album page's
    /// (`album_qt::header_tint_hex`) — the Slint derives both from the SAME
    /// `artwork::header_tint`, so there is one colour pipeline, not two.
    #[serde(rename = "headerColor")]
    pub header_color: String,
    /// `album_header_gradient` as of page load; the view prefers the live
    /// settings snapshot and falls back to this (see album_qt.rs).
    #[serde(rename = "headerGradient")]
    pub header_gradient: bool,
}

fn mmss(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// artist.rs `map_release`.
pub(crate) fn map_release(release: &PageArtistRelease) -> AlbumCardData {
    let artist = release
        .artist
        .as_ref()
        .map(|a| a.name.display.clone())
        .or_else(|| {
            release
                .artists
                .as_ref()
                .and_then(|list| list.first().map(|a| a.name.clone()))
        })
        .unwrap_or_default();
    let artist_id = release
        .artist
        .as_ref()
        .map(|a| a.id.to_string())
        .unwrap_or_default();
    let bit_depth = release
        .audio_info
        .as_ref()
        .and_then(|a| a.maximum_bit_depth);
    let sample_rate = release
        .audio_info
        .as_ref()
        .and_then(|a| a.maximum_sampling_rate);
    AlbumCardData {
        is_pinned: crate::sidebar_qt::is_pinned("album", &release.id),
        // Heart from the favourite-id cache — the artist page's Releases grid
        // mounts the same AlbumCard as Home (see `AlbumCardData::is_favorite`).
        is_favorite: crate::fav_cache_qt::is_album_favorite(&release.id),
        id: release.id.clone(),
        // The Qobuz `version` ("50th Anniversary", "Deluxe") rides the title
        // everywhere else in the app (`format_album_title`); the card is not
        // the exception — a title without it points at the wrong edition.
        title: crate::album_qt::format_album_title(&release.title, release.version.as_deref()),
        artist,
        artist_id,
        genre: release
            .genre
            .as_ref()
            .map(|g| g.name.clone())
            .unwrap_or_default(),
        // Localized "Sep 2, 2021" like every other card in the app — this
        // slot is display text, not a sort key (the numeric-year sites in
        // this file and in myqbz_builder_fetch keep their `i32` on purpose).
        year: qbz_text_utils::dates::release_label(
            release.dates.as_ref().and_then(|d| d.original.as_deref()),
        ),
        default_order: 0,
        release_sort_key: qbz_text_utils::dates::release_sort_key(
            release.dates.as_ref().and_then(|d| d.original.as_deref()),
        ),
        quality_tier: home_qt::quality_tier_from_depth(bit_depth).to_string(),
        quality_detail: home_qt::quality_detail_from_parts(bit_depth, sample_rate),
        art_url: crate::cover_artwork_qt::prefer_album_cover(
            &release.id,
            // Discography grid card: full variant (best()) — the down-tier
            // was reverted after the 2026-08-15 owner smoke (contract 04 §3).
            release
                .image
                .as_ref()
                .and_then(|img| img.best().cloned())
                .unwrap_or_default(),
        ),
    }
}

/// `album_map::sort_album_items` (crates/qbz/src/album_map.rs:244-264) for the
/// artist page's release buckets — the SAME function `label_qt::sort_cards`
/// already ports for `HomeCard`, here for `AlbumCardData`.
///
/// `release_sort_key` is the original date, independent of the localized
/// display label in `year`. Missing dates sort last in either direction.
/// `title`/`artist` are case-insensitive, per the reference's doc comment.
///
/// "default" restores the server ordinal captured before applying any saved
/// sort. Unknown keys leave the vector untouched. The default preference is
/// represented by absence in artist_prefs.
///
/// `artist-asc` / `artist-desc` are unreachable from the artist page's picker
/// (its five options stop at title-desc) but are carried anyway so this stays
/// 1:1 with `sort_album_items`.
///
/// The discography page has its OWN copy of these arms
/// (`artist_releases_qt::sort_cards`) rather than reusing this one: its rows
/// are `home_qt::HomeCard` (they must carry `artPath` for `AlbumCollection`),
/// not `AlbumCardData`, so the two differ in their item type and nothing else.
/// `label_qt::sort_cards` is the third copy, for the same reason.
pub(crate) fn sort_release_cards(items: &mut [AlbumCardData], sort: &str) {
    match sort {
        "default" | "relevant" => items.sort_by_key(|card| card.default_order),
        "oldest" | "year-asc" => items.sort_by(|a, b| {
            qbz_text_utils::dates::compare_release_dates(
                a.release_sort_key,
                b.release_sort_key,
                false,
            )
        }),
        "newest" | "year-desc" => items.sort_by(|a, b| {
            qbz_text_utils::dates::compare_release_dates(
                a.release_sort_key,
                b.release_sort_key,
                true,
            )
        }),
        "title-asc" => items.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
        "title-desc" => items.sort_by(|a, b| b.title.to_lowercase().cmp(&a.title.to_lowercase())),
        "artist-asc" => items.sort_by(|a, b| a.artist.to_lowercase().cmp(&b.artist.to_lowercase())),
        "artist-desc" => {
            items.sort_by(|a, b| b.artist.to_lowercase().cmp(&a.artist.to_lowercase()))
        }
        _ => {}
    }
}

/// `sort_release_cards` over the STASHED document's `cards` array, which is raw
/// JSON rather than typed rows (the stash is a `serde_json::Value` — see
/// `ARTIST_DOC`).
///
/// The round-trip through `AlbumCardData` is deliberate: it keeps ONE copy of
/// the sort arms above instead of a second comparator written against
/// `Value`, and these arrays are at most a few dozen rows. A row that fails to
/// deserialize leaves the whole bucket untouched rather than half-sorted — the
/// document is what the view paints, and a partial re-order would be worse
/// than none.
fn sort_release_values(items: &mut [serde_json::Value], sort: &str) {
    let parsed: Result<Vec<AlbumCardData>, _> = items
        .iter()
        .map(|v| serde_json::from_value::<AlbumCardData>(v.clone()))
        .collect();
    let mut cards = match parsed {
        Ok(cards) => cards,
        Err(e) => {
            log::warn!("[qbz-qt] release bucket not sortable ({e}) — leaving its order alone");
            return;
        }
    };
    sort_release_cards(&mut cards, sort);
    for (slot, card) in items.iter_mut().zip(cards.iter()) {
        match serde_json::to_value(card) {
            Ok(value) => *slot = value,
            Err(e) => log::warn!("[qbz-qt] release card re-serialize failed: {e}"),
        }
    }
}

/// artist.rs `map_track` (Popular Tracks / Appears On rows — album title +
/// cover ride the nested album object).
fn map_track(index: usize, track: PageArtistTrack) -> TrackRow {
    let mut title = track.title;
    if let Some(version) = track.version.as_ref().filter(|v| !v.is_empty()) {
        title = format!("{title} ({version})");
    }
    let (artist, artist_id) = track
        .artist
        .map(|a| (a.name.display, a.id.to_string()))
        .unwrap_or_default();
    let (album_id, album, artwork_url) = track
        .album
        .map(|a| {
            let url = a
                .image
                .and_then(|img| img.smallest().cloned())
                .unwrap_or_default();
            (a.id, a.title, url)
        })
        .unwrap_or_default();
    let bit_depth = track.audio_info.as_ref().and_then(|a| a.maximum_bit_depth);
    let sample_rate = track
        .audio_info
        .as_ref()
        .and_then(|a| a.maximum_sampling_rate);
    // /artist/page reports availability on the nested `rights` object, not as a
    // flat key — the same read `playback_qt::artist_top_queue_tracks` (:731)
    // already does when it builds the queue row from this payload. `unwrap_or
    // (true)` is §3.1's rule at the parse site: a thin page payload must never
    // grey out an artist's whole track list.
    let not_streamable = !track
        .rights
        .as_ref()
        .and_then(|r| r.streamable)
        .unwrap_or(true);
    TrackRow {
        featured: String::new(),
        is_favorite: crate::fav_cache_qt::contains_track(track.id),
        id: track.id.to_string(),
        cache_status: if crate::offline_qt::is_cached(&track.id.to_string()) {
            3
        } else {
            0
        },
        number: (index + 1).to_string(),
        title,
        artist,
        artist_id,
        album,
        album_id,
        duration: mmss(track.duration.unwrap_or(0)),
        quality_tier: home_qt::quality_tier_from_depth(bit_depth).to_string(),
        quality_detail: home_qt::quality_detail_from_parts(bit_depth, sample_rate),
        // The RAW numbers ride with the row (see `TrackRow::bit_depth`) so
        // `track_row_to_queue` can fill the queue entry the way the reference's
        // `make_top_track_queue` (playback.rs:3188) fills it off `audio_info`.
        bit_depth,
        sample_rate,
        explicit: track.parental_warning.unwrap_or(false),
        disc: 1,
        work_header: String::new(),
        work_composer_name: String::new(),
        work_composer_id: String::new(),
        artwork_url,
        not_streamable,
        // /artist/page track items carry neither `streamable_at` nor a
        // release date (only the nested `rights`), so an unavailable row here
        // keeps the generic "no longer available" wording.
        upcoming: false,
    }
}

/// Fetch and map an artist page by id (artist.rs `load_artist`/`map_artist`).
pub async fn load_artist(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    artist_id: &str,
) -> Result<ArtistViewData, String> {
    load_artist_with_evidence(runtime, artist_id)
        .await
        .map(|(data, _)| data)
}

/// The raw identity evidence the Artist Page hands the MusicBrainz source
/// resolver (#768): the page's Qobuz id and the ISRCs of its OWN Popular
/// Tracks. Collected off the raw `/artist/page` response before the lossy
/// `map_artist`, so no field is added to the rendered document and no second
/// Qobuz request is made. Never reaches QML.
pub(crate) struct SourceEvidence {
    pub qobuz_id: u64,
    pub isrcs: Vec<String>,
}

pub(crate) async fn load_artist_with_evidence(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    artist_id: &str,
) -> Result<(ArtistViewData, SourceEvidence), String> {
    let id: u64 = artist_id
        .parse()
        .map_err(|_| format!("invalid artist id: {artist_id}"))?;
    let page = runtime
        .core()
        .get_artist_page(id, None)
        .await
        .map_err(|e| e.to_string())?;
    let isrcs = qbz_integrations::musicbrainz::identity::pick_source_isrcs(
        page.id,
        page.top_tracks
            .iter()
            .flatten()
            .map(|t| (t.isrc.as_deref(), t.artist.as_ref().map(|a| a.id))),
        qbz_integrations::musicbrainz::identity::MAX_SOURCE_ISRCS,
    );
    let evidence = SourceEvidence {
        qobuz_id: page.id,
        isrcs,
    };
    Ok((map_artist(page), evidence))
}

fn map_artist(page: PageArtistResponse) -> ArtistViewData {
    let name = page.name.display;
    // Biography: content (HTML-stripped) + the attribution name. Qobuz sends
    // `biography.source` as a raw JSON value (sometimes a string, sometimes an
    // object) — only the string form carries an attribution.
    let (bio, bio_source) = match page.biography {
        Some(biography) => {
            let content = biography
                .content
                .map(|c| qbz_text_utils::strip_html::strip_html(&c))
                .unwrap_or_default();
            let source = biography
                .source
                .and_then(|v| {
                    v.as_str()
                        .map(qbz_text_utils::strip_html::decode_html_entities)
                })
                .unwrap_or_default();
            (content, source)
        }
        None => (String::new(), String::new()),
    };
    let bio_short = truncate_words(&bio, 360);
    let bio_truncated = bio_short != bio;
    let artwork_url = page
        .images
        .and_then(|images| images.portrait)
        .map(|portrait| {
            format!(
                "https://static.qobuz.com/images/artists/covers/large/{}.{}",
                portrait.hash, portrait.format
            )
        })
        .unwrap_or_default();

    // Custom portrait — the SHARED `custom_artwork.json` store, keyed by
    // artist NAME exactly as `ArtistPageView.slint:312` keys it (see the
    // module header of cover_artwork_qt.rs for why id-keying is wrong here).
    // The `is_file` filter is the reference's accepted behaviour: an override
    // whose file the user has since moved simply stops applying.
    let custom_image_path = crate::cover_artwork_qt::artist_image(&name)
        .filter(|p| std::path::Path::new(p).is_file())
        .unwrap_or_default();
    let has_custom_image = !custom_image_path.is_empty();
    // Backfill the hash -> override link (the same backfill album_qt.rs does
    // on album load) so a portrait set in the Slint build, or before this map
    // existed, propagates to every OTHER surface that only knows the URL.
    if has_custom_image && !artwork_url.is_empty() {
        crate::cover_artwork_qt::note_override_key(&artwork_url, &custom_image_path);
    }

    let top_tracks = page
        .top_tracks
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(index, track)| map_track(index, track))
        .collect();

    // Server-driven bucketing (webplayer source of truth): every non-empty
    // bucket in the official order; ids deduped across groups; labels from
    // the artist's own "album" group.
    let mut bucket_cards: HashMap<String, Vec<AlbumCardData>> = HashMap::new();
    let mut bucket_has_more: HashMap<String, bool> = HashMap::new();
    let mut seen_release_ids: HashSet<String> = HashSet::new();
    let mut labels_by_id: BTreeMap<u64, String> = BTreeMap::new();

    for group in page.releases.into_iter().flatten() {
        let release_type = group.release_type.clone();
        let is_album_group = release_type == "album";
        *bucket_has_more.entry(release_type.clone()).or_insert(false) |= group.has_more;
        for release in group.items.into_iter() {
            if seen_release_ids.contains(&release.id) {
                continue;
            }
            seen_release_ids.insert(release.id.clone());
            if is_album_group {
                if let Some(label) = release.label.as_ref() {
                    labels_by_id
                        .entry(label.id)
                        .or_insert_with(|| label.name.clone());
                }
            }
            bucket_cards
                .entry(release_type.clone())
                .or_default()
                .push(map_release(&release));
        }
    }

    let mut labels: Vec<ArtistLabel> = labels_by_id
        .into_iter()
        .map(|(id, name)| ArtistLabel {
            id: id.to_string(),
            name,
        })
        .collect();
    labels.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let similar_artists: Vec<ArtistSimilar> = page
        .similar_artists
        .map(|s| {
            s.items
                .into_iter()
                .map(|item| ArtistSimilar {
                    id: item.id.to_string(),
                    name: item.name.display,
                })
                .collect()
        })
        .unwrap_or_default();

    let playlists: Vec<ArtistPlaylist> = page
        .playlists
        .map(|p| {
            p.items
                .into_iter()
                .map(|pl| {
                    // `owner` carries BOTH halves this row needs — the display
                    // name for the subtitle and the id for the ownership
                    // check — so read the id off the borrow before the name
                    // moves it out.
                    let owner_id = pl.owner.as_ref().map(|o| o.id).unwrap_or(0);
                    let owner = pl
                        .owner
                        .and_then(|o| o.name)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "Qobuz".to_string());
                    let track_count = pl.tracks_count.unwrap_or(0);
                    let art_url = pl
                        .images
                        .and_then(|imgs| imgs.rectangle)
                        .and_then(|rects| rects.into_iter().find(|s| !s.is_empty()))
                        .unwrap_or_default();
                    ArtistPlaylist {
                        is_pinned: crate::sidebar_qt::is_pinned("playlist", &pl.id.to_string()),
                        is_favorite: crate::fav_cache_qt::is_playlist_favorite(pl.id),
                        playlist_owned: crate::playlist_qt::owns(owner_id),
                        playlist_following: crate::playlist_qt::is_following(pl.id),
                        // `images.rectangle` is the playlist's own graphic by
                        // definition — `PageArtistPlaylistImages` has no other
                        // field (qbz-models types.rs:1436-1439).
                        playlist_own_image: !art_url.is_empty(),
                        id: pl.id.to_string(),
                        title: pl.title.unwrap_or_default(),
                        subtitle: format!("{owner} · {track_count}"),
                        art_url,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let last_release = page.last_release.as_ref().map(map_release);
    let appears_on: Vec<TrackRow> = page
        .tracks_appears_on
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, track)| map_track(index, track))
        .collect();

    // The persisted per-bucket sorts, read ONCE for the whole page (the Slint
    // calls `artist_prefs::get_sort` per bucket off a thread_local cache;
    // this port has no cache by design — see artist_prefs.rs's header).
    let saved_sorts = crate::artist_prefs::read_all();
    let sort_for = |rt: &str| {
        saved_sorts
            .get(rt)
            .cloned()
            .unwrap_or_else(|| crate::artist_prefs::DEFAULT_SORT.to_string())
    };

    // One section per non-empty bucket in the official order; "download"
    // drains but never renders (Purchase Only is hidden upstream too).
    let mut release_sections: Vec<ArtistReleaseSection> = Vec::new();
    for &(rt, title) in RELEASE_SECTION_ORDER {
        if rt == "download" {
            bucket_cards.remove(rt);
            continue;
        }
        if let Some(mut cards) = bucket_cards.remove(rt) {
            if cards.is_empty() {
                continue;
            }
            // artist.rs:786-812 — the sort is applied at page BUILD, not on
            // first click, "so the first paint already honors the user's
            // choice", and `sort_by` is stamped so the picker seats itself on
            // that choice instead of snapping back to Default on a revisit.
            for (index, card) in cards.iter_mut().enumerate() {
                card.default_order = index as u32;
            }
            let sort = sort_for(rt);
            sort_release_cards(&mut cards, &sort);
            release_sections.push(ArtistReleaseSection {
                release_type: rt.to_string(),
                title: qbz_i18n::t(title),
                has_more: bucket_has_more.get(rt).copied().unwrap_or(false),
                cards,
                sort_by: sort,
            });
        }
    }
    // Leftovers: server buckets unknown to the table, appended (title-cased).
    // These render a full ReleaseSection header, sort picker included, so they
    // must round-trip the pref exactly like the known buckets.
    for (rt, mut cards) in bucket_cards {
        if cards.is_empty() {
            continue;
        }
        // Factored into `title_case` (above) when the discography page needed
        // the same fallback — one copy, so a leftover bucket's section header
        // and its discography page header cannot drift apart.
        let title = title_case(&rt);
        for (index, card) in cards.iter_mut().enumerate() {
            card.default_order = index as u32;
        }
        let sort = sort_for(&rt);
        sort_release_cards(&mut cards, &sort);
        release_sections.push(ArtistReleaseSection {
            has_more: false,
            release_type: rt,
            title,
            cards,
            sort_by: sort,
        });
    }

    // The library rows are feed-only by nature (they ARE the owned items), but
    // the follow state must not be: the feed is empty until the Library view
    // is opened, so this drew an un-followed heart on artists the user follows
    // — and `toggle_favorite` then read the same false and re-added the
    // follow. `library_qt::is_favorite` checks the favourite-id cache first.
    //
    // The feed's coldness is handled one level up: `main::open_artist` warms
    // it in the background and `refresh_library_items` repaints this pair
    // when it lands, so a fresh session (or a fresh account) no longer opens
    // every artist with an invisible "In library" toggle.
    let library_items = crate::library_qt::artist_library_items(&page.id.to_string());
    let library_count = library_items.len() as i64;
    log_library_membership(&page.id.to_string(), &library_items);
    let is_following = crate::library_qt::is_favorite("artist", &page.id.to_string());

    ArtistViewData {
        id: page.id.to_string(),
        name,
        bio,
        bio_short,
        bio_truncated,
        bio_source,
        artwork_url,
        has_custom_image,
        custom_image_url: if custom_image_path.is_empty() {
            String::new()
        } else {
            crate::artwork_qt::file_url(&custom_image_path)
        },
        custom_image_path,
        is_following,
        is_pinned: crate::sidebar_qt::is_pinned("artist", &page.id.to_string()),
        is_blacklisted: crate::artist_blacklist::is_blacklisted(page.id),
        library_count,
        library_items,
        library_sort: crate::artist_prefs::get_sort("library"),
        top_tracks,
        appears_on,
        last_release,
        release_sections,
        labels,
        similar_artists,
        playlists,
        // Filled by the background enrichment pass (see load_artist_view).
        stories: Vec::new(),
        stories_loading: false,
        network: ArtistNetwork::default(),
        // Header atmosphere: the tint resolves from this page's artwork on a
        // background pass (spawn_header_color), so the first publish carries
        // the empty value and the gradient fades in with it.
        header_color: String::new(),
        header_gradient: false,
    }
}

/// The last loaded artist's Popular Tracks as a playable queue (row play
/// + Play-all + Shuffle-all + Play-next/queue on rows).
static TOP_QUEUE: std::sync::Mutex<Vec<QueueTrack>> = std::sync::Mutex::new(Vec::new());

fn track_row_to_queue(row: &TrackRow) -> QueueTrack {
    let duration_secs = {
        let mut parts = row.duration.split(':');
        parts
            .next()
            .and_then(|m| m.parse::<u64>().ok())
            .unwrap_or(0)
            * 60
            + parts
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
    };
    QueueTrack {
        id: row.id.parse().unwrap_or(0),
        title: row.title.clone(),
        version: None,
        artist: row.artist.clone(),
        album: row.album.clone(),
        album_version: None,
        duration_secs,
        artwork_url: if row.artwork_url.is_empty() {
            None
        } else {
            Some(row.artwork_url.clone())
        },
        // playback.rs `make_top_track_queue` (:3188): the CATALOG max travels
        // with the queue track. `None` here zeroed `quality_state`'s
        // `TRACK_MAX_*` seed, so the NPB AudioStamp drew a bare tier with no
        // "24-bit / 96 kHz" line on every artist Popular-Tracks play.
        hires: row.quality_tier == "hires",
        bit_depth: row.bit_depth,
        sample_rate: row.sample_rate,
        is_local: false,
        album_id: if row.album_id.is_empty() {
            None
        } else {
            Some(row.album_id.clone())
        },
        artist_id: row.artist_id.parse::<u64>().ok(),
        // D5: carried from the row, which read it off `/artist/page`'s nested
        // `rights.streamable` (see `map_track`). Hardcoding `true` here threw
        // away an answer the payload had already given us.
        streamable: !row.not_streamable,
        source: Some("qobuz".to_string()),
        parental_warning: row.explicit,
        source_item_id_hint: None,
        context_kind: None,
        context_id: None,
        isrc: None,
        recording_mbid: None,
    }
}

/// Stash the queue at publish time (called from load_artist_view).
///
/// The queue is born STAMPED with the PAGE's artist — playback.rs stamps
/// ("artist", artist_id) at every artist play path (:2848 Popular, :3108
/// shuffled, :3170 from-row) and never derives it. Deriving is wrong here in
/// two ways: `derive_context` returns the shared ALBUM before it ever tests the
/// artist (so an artist whose Popular Tracks come from one album lands on that
/// album), and `TrackRow.artist_id` is the track's OWN credited performer
/// (`map_track`), so a single featured collaboration makes the queue share no
/// artist and lose its origin entirely. `data.id` is the page id — the thing
/// the user actually launched.
fn stash_top_queue(data: &ArtistViewData) {
    let mut queue: Vec<QueueTrack> = data.top_tracks.iter().map(track_row_to_queue).collect();
    if !data.id.is_empty() {
        for track in queue.iter_mut() {
            track.context_kind = Some("artist".to_string());
            track.context_id = Some(data.id.clone());
        }
    }
    *TOP_QUEUE.lock().unwrap() = queue;
}

/// The stashed queue (cloned) + the start index for `track_id` (0 when
/// unknown — e.g. Play-all).
pub fn top_queue(track_id: Option<u64>) -> (Vec<QueueTrack>, usize) {
    let queue = TOP_QUEUE.lock().unwrap().clone();
    let start = track_id
        .and_then(|id| queue.iter().position(|t| t.id == id))
        .unwrap_or(0);
    (queue, start)
}

/// Popularity is server-ranked; the other choices sort release-date pages locally.
pub(crate) fn release_server_sort(sort: &str) -> &'static str {
    if sort == "relevant" {
        "relevant"
    } else {
        "release_date"
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseRequest {
    generation: u64,
    revision: u64,
    order: String,
}

fn section_request(generation: u64, section: &serde_json::Value) -> ReleaseRequest {
    ReleaseRequest {
        generation,
        revision: section["sortRevision"].as_u64().unwrap_or(0),
        order: release_server_sort(section["sortBy"].as_str().unwrap_or("default")).into(),
    }
}

fn release_request(artist_id: &str, release_type: &str) -> Option<ReleaseRequest> {
    let guard = ARTIST_DOC.lock().ok()?;
    let (generation, doc) = guard.as_ref()?;
    if doc["id"].as_str() != Some(artist_id) {
        return None;
    }
    let section = doc["releaseSections"]
        .as_array()?
        .iter()
        .find(|s| s["releaseType"].as_str() == Some(release_type))?;
    Some(section_request(*generation, section))
}

pub(crate) fn release_request_is_current(
    artist_id: &str,
    release_type: &str,
    request: &ReleaseRequest,
) -> bool {
    release_request(artist_id, release_type).as_ref() == Some(request)
}

async fn fetch_release_cards(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    artist_id: &str,
    release_type: &str,
    offset: u32,
    order: &str,
) -> Result<(Vec<AlbumCardData>, bool), String> {
    let id = artist_id
        .parse()
        .map_err(|_| format!("invalid artist id: {artist_id}"))?;
    let resp = runtime
        .core()
        .get_releases_grid(id, release_type, RELEASE_PAGE_SIZE, offset, Some(order))
        .await
        .map_err(|e| e.to_string())?;
    let has_more = resp.has_more && !resp.items.is_empty();
    let cards = resp
        .items
        .iter()
        .enumerate()
        .map(|(index, release)| {
            let mut card = map_release(release);
            card.default_order = offset.saturating_add(index as u32);
            card
        })
        .collect();
    Ok((cards, has_more))
}

pub(crate) async fn load_release_page(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    artist_id: &str,
    release_type: &str,
    offset: u32,
) -> Result<Option<(Vec<AlbumCardData>, bool, ReleaseRequest)>, String> {
    let Some(request) = release_request(artist_id, release_type) else {
        return Ok(None);
    };
    let (cards, has_more) =
        fetch_release_cards(runtime, artist_id, release_type, offset, &request.order).await?;
    if !release_request_is_current(artist_id, release_type, &request) {
        return Ok(None);
    }
    merge_release_page(artist_id, release_type, &cards, has_more, &request);
    Ok(Some((cards, has_more, request)))
}

/// Append `cards` to `release_type`'s bucket inside the stashed document
/// (no republish — the caller's `releaseSectionReady` signal already paints
/// them; this only keeps a LATER republish from losing them). Ignored when
/// the stashed document is for a different artist (the user navigated away
/// while the page was in flight).
fn merge_release_page(
    artist_id: &str,
    release_type: &str,
    cards: &[AlbumCardData],
    has_more: bool,
    request: &ReleaseRequest,
) {
    let Ok(mut guard) = ARTIST_DOC.lock() else {
        return;
    };
    let Some((generation, doc)) = guard.as_mut() else {
        return;
    };
    if doc.get("id").and_then(|v| v.as_str()) != Some(artist_id) {
        return;
    }
    let Some(sections) = doc
        .get_mut("releaseSections")
        .and_then(|v| v.as_array_mut())
    else {
        return;
    };
    let Some(section) = sections
        .iter_mut()
        .find(|s| s.get("releaseType").and_then(|v| v.as_str()) == Some(release_type))
    else {
        return;
    };
    if section_request(*generation, section) != *request {
        return;
    }
    section["hasMore"] = json!(has_more);
    // The bucket's live sort, read off the row itself — artist.rs:1245-1258
    // `append_release_page` does exactly this (`let sort = row.sort_by...`)
    // before re-sorting, because under a custom sort a page appended at the
    // TAIL is simply in the wrong place.
    let sort = section
        .get("sortBy")
        .and_then(|v| v.as_str())
        .unwrap_or(crate::artist_prefs::DEFAULT_SORT)
        .to_string();
    let Some(existing) = section.get_mut("cards").and_then(|v| v.as_array_mut()) else {
        return;
    };
    let seen: HashSet<String> = existing
        .iter()
        .filter_map(|c| c.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    for card in cards {
        if seen.contains(&card.id) {
            continue;
        }
        if let Ok(value) = serde_json::to_value(card) {
            existing.push(value);
        }
    }
    // No-op under "default" (the whole point of that key), so the common path
    // pays nothing.
    sort_release_values(existing, &sort);
}

/// `(generation, artist id)` of the page on screen, or `None` when no artist
/// document is stashed.
fn stashed_page() -> Option<(u64, String)> {
    let guard = ARTIST_DOC.lock().ok()?;
    let (generation, doc) = guard.as_ref()?;
    let id = doc.get("id").and_then(|v| v.as_str())?.to_string();
    Some((*generation, id))
}

// ==================== "In library" tab: live membership ====================
//
// The tab's rows travel inside the artist document (`ArtistViewData::
// library_items`). Two things move them after the page is built, and both
// come through `publish_patch` so the generation guard applies:
//
//   * the Library feed landing AFTER the page opened (`main::open_artist`
//     warms it; `main::reload_library` calls `refresh_library_items` when it
//     is published), and
//   * a heart settling on an album / track (`main::emit_library_favorite` ->
//     `apply_favorite_change`), after `library_qt::reconcile_qobuz_favorite`
//     has inserted or removed the feed row.
//
// Both recompute the WHOLE list off the feed rather than patching one row:
// the rule (contract §3 — purchases ∪ favourites, one row per entity, the
// purchase row representative) is easier to keep in one place than to
// replay incrementally, and the scan is a single pass over the feed.

/// Recompute the open page's library rows from the cached feed and republish
/// `libraryItems` + `libraryCount`. No-op without an artist on screen or
/// before the Library has ever loaded (the count then stays at what the page
/// was built with — 0 on a cold session — until the feed lands and this runs
/// again).
pub(crate) fn refresh_library_items() {
    let Some((generation, artist_id)) = stashed_page() else {
        return;
    };
    if crate::library_qt::with_library(|_| ()).is_none() {
        return;
    }
    let items = crate::library_qt::artist_library_items(&artist_id);
    log_library_membership(&artist_id, &items);
    let count = items.len() as i64;
    let items_json = match serde_json::to_value(&items) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[qbz-qt] artist library rows serialize failed: {e}");
            return;
        }
    };
    publish_patch(generation, move |doc| {
        if let Some(obj) = doc.as_object_mut() {
            obj.insert("libraryItems".into(), items_json);
            obj.insert("libraryCount".into(), json!(count));
        }
    });
}

/// A heart settled on an album or track: the tab's membership may have
/// changed (a new favourite joins, a cleared one leaves unless it is also a
/// purchase). `main::emit_library_favorite` is the one door, the same fan-out
/// `home_qt` / `search_qt` / `recommendations_qt` hang off; the artist page was
/// listed there as "NOT yet covered".
pub(crate) fn apply_favorite_change(kind: &str, _id: &str, _value: bool) {
    if kind == "track" || kind == "album" {
        refresh_library_items();
    }
}

/// Contract §5 discriminator, kept as `debug` once the round closed: how the
/// open artist's owned rows split by kind and by which id matched. Read it
/// with `RUST_LOG=qbz_qt=debug` when an artist's tab looks short.
fn log_library_membership(artist_id: &str, items: &[FeedItem]) {
    if !log::log_enabled!(log::Level::Debug) {
        return;
    }
    let tracks = items.iter().filter(|i| i.kind == "track").count();
    let albums = items.len() - tracks;
    let by_album_artist = items
        .iter()
        .filter(|i| i.kind == "track" && i.artist_id != artist_id)
        .count();
    log::debug!(
        "[qbz-qt] artist {artist_id} in-library: {tracks} track(s) ({by_album_artist} via album \
         artist), {albums} album(s)"
    );
}

/// Local sorts retain loaded pages. Switching to/from server popularity
/// starts at offset zero and invalidates replies from the previous order.
pub(crate) fn resort_section(release_type: &str, sort: &str) {
    crate::artist_prefs::set_sort(release_type, sort);
    let target = ARTIST_DOC.lock().ok().and_then(|guard| {
        let (generation, doc) = guard.as_ref()?;
        Some((*generation, doc["id"].as_str()?.to_string()))
    });
    let Some((generation, artist_id)) = target else {
        return;
    };
    let release_type = release_type.to_string();
    let sort = sort.to_string();
    let mut reload = None;
    publish_patch(generation, |doc| {
        let Some(section) = doc["releaseSections"].as_array_mut().and_then(|sections| {
            sections
                .iter_mut()
                .find(|s| s["releaseType"].as_str() == Some(&release_type))
        }) else {
            return;
        };
        let old = section["sortBy"].as_str().unwrap_or("default");
        let changed = release_server_sort(old) != release_server_sort(&sort);
        section["sortBy"] = json!(sort);
        if changed {
            section["sortRevision"] = json!(section["sortRevision"]
                .as_u64()
                .unwrap_or(0)
                .wrapping_add(1));
            section["cards"] = json!([]);
            section["hasMore"] = json!(true);
            section["sortLoading"] = json!(true);
            reload = Some(section_request(generation, section));
        } else if let Some(cards) = section["cards"].as_array_mut() {
            sort_release_values(cards, &sort);
        }
    });
    let Some(request) = reload else {
        return;
    };
    crate::spawn(async move {
        let result =
            fetch_release_cards(&crate::app(), &artist_id, &release_type, 0, &request.order).await;
        publish_patch(generation, |doc| {
            let Some(section) = doc["releaseSections"].as_array_mut().and_then(|sections| {
                sections
                    .iter_mut()
                    .find(|s| s["releaseType"].as_str() == Some(&release_type))
            }) else {
                return;
            };
            if section_request(generation, section) != request {
                return;
            }
            section["sortLoading"] = json!(false);
            match result {
                Ok((mut cards, has_more)) => {
                    sort_release_cards(&mut cards, section["sortBy"].as_str().unwrap_or("default"));
                    section["cards"] = json!(cards);
                    section["hasMore"] = json!(has_more);
                }
                Err(e) => log::warn!("[qbz-qt] artist release sort failed: {e}"),
            }
        });
    });
}

/// Fetch + publish (perf-marked like phase 5), then kick the progressive
/// enrichment (Magazine stories + the MusicBrainz Network sidebar) off in the
/// background. The returned JSON is what main.rs writes onto the bridge; every
/// later pass re-publishes the SAME document through `publish_patch`.
pub async fn load_artist_view(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    artist_id: &str,
) -> Result<String, String> {
    let t = Instant::now();
    let (mut data, evidence) = load_artist_with_evidence(runtime, artist_id).await?;
    // The embedded artist/page buckets use release-date order. A persisted
    // popularity choice must fetch the server ranking before the first paint.
    for section in &mut data.release_sections {
        if section.sort_by == "relevant" {
            let (cards, has_more) =
                fetch_release_cards(runtime, artist_id, &section.release_type, 0, "relevant")
                    .await?;
            section.cards = cards;
            section.has_more = has_more;
        }
    }

    let sections: usize = data.release_sections.iter().map(|s| s.cards.len()).sum();
    stash_top_queue(&data);

    // Seed the sidebar's own loading flags BEFORE serializing so the sections
    // paint their "Loading…" state with the first frame. `mb_available` is the
    // user's MusicBrainz opt-in read straight from the core: false here means
    // the enrichment task never even starts, so no MB request is issued.
    let mb_on = runtime.core().musicbrainz_is_enabled().await;
    data.network.mb_available = mb_on;
    data.network.origin_loading = mb_on;
    data.network.relationships_loading = mb_on;
    data.network.discovery_loading = mb_on;
    data.stories_loading = true;
    // Header atmosphere — see the ArtistViewData field comments.
    data.header_gradient = crate::settings_qt::pref_bool("album_header_gradient", true);
    // Tint + atmosphere come from the image the user actually SEES. The
    // override's raw path is handed over directly rather than relying on the
    // hash link, because an artist Qobuz has no portrait for has an empty
    // `artwork_url` — the very case a custom portrait exists for, and the one
    // `spawn_header_color` early-returns on.
    let header_art = if data.custom_image_path.is_empty() {
        data.artwork_url.clone()
    } else {
        data.custom_image_path.clone()
    };

    let json = serde_json::to_string(&data).map_err(|e| e.to_string())?;
    log::info!(
        "[qbz-qt][perf] artist load: {:?} ({} top tracks, {} releases in {} sections)",
        t.elapsed(),
        data.top_tracks.len(),
        sections,
        data.release_sections.len(),
    );

    // Stash the document for the progressive passes, under a fresh generation.
    let generation = ARTIST_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    match serde_json::from_str::<serde_json::Value>(&json) {
        Ok(doc) => {
            if let Ok(mut guard) = ARTIST_DOC.lock() {
                *guard = Some((generation, doc));
            }
        }
        Err(e) => log::warn!("[qbz-qt] artist doc stash failed: {e}"),
    }

    spawn_header_color(generation, header_art);

    let similar_names: Vec<String> = data
        .similar_artists
        .iter()
        .map(|s| s.name.clone())
        .collect();
    spawn_enrichment(
        runtime.clone(),
        generation,
        artist_id.to_string(),
        data.name.clone(),
        evidence,
        similar_names,
        mb_on,
    );

    Ok(json)
}

// ==================== Progressive enrichment (stories + MusicBrainz) =======

/// Monotonic id for the artist page currently on screen. Every background
/// pass carries the generation it was started for and is dropped when the
/// user has navigated on — a slow MB reply can never repaint a later artist.
static ARTIST_GEN: AtomicU64 = AtomicU64::new(0);
/// The last published artist document, kept so a partial update can be merged
/// into it and the WHOLE document re-published (the bridge carries exactly one
/// JSON property, phase-23 pattern).
static ARTIST_DOC: Mutex<Option<(u64, serde_json::Value)>> = Mutex::new(None);

/// Merge `f`'s edits into the stashed document and re-publish it, but only
/// while `generation` is still the page on screen.
fn publish_patch(generation: u64, f: impl FnOnce(&mut serde_json::Value)) {
    let json = {
        let Ok(mut guard) = ARTIST_DOC.lock() else {
            return;
        };
        let Some((current, doc)) = guard.as_mut() else {
            return;
        };
        if *current != generation {
            return;
        }
        f(doc);
        match serde_json::to_string(&*doc) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("[qbz-qt] artist doc republish failed: {e}");
                return;
            }
        }
    };
    crate::artist_bridge::ui(move |mut b| {
        b.as_mut().set_artist_json(QString::from(json.as_str()));
    });
}

/// Repaint the OPEN artist page after its custom portrait was set or removed
/// (`cover_artwork_qt::add_custom_artist_image` / `remove_custom_artist_image`).
///
/// DELIBERATELY NOT `crate::open_artist`. That router
/// (`main.rs:1066-1075`) records a search-ranking interaction, then returns
/// early when the session is offline, then records a nav entry — so re-opening
/// would (a) do NOTHING at all offline, leaving a picked portrait invisible,
/// (b) push a duplicate nav entry so Back re-lands on the same artist, and
/// (c) log a page interaction the user never performed. This patches the two
/// published fields into the stashed document and re-publishes it through
/// `publish_patch`, the port's ONE transport for this view — the same shape
/// `resort_section` uses, and the same instinct as the Slint reference, which
/// applies the new image in place without a reload
/// (`crates/qbz/src/main.rs:23184-23187`).
///
/// Guarded on the artist NAME rather than the generation counter, for the
/// reason `resort_section` reads its generation off the stash: the click was
/// aimed at the page on screen, and if the user has navigated on since the
/// file picker opened, the stash belongs to somebody else and the edit is
/// dropped.
pub(crate) fn apply_custom_image(artist_name: &str) {
    let path = crate::cover_artwork_qt::artist_image(artist_name)
        .filter(|p| std::path::Path::new(p).is_file())
        .unwrap_or_default();
    // The guard is dropped before `publish_patch` takes the same lock.
    let stash = {
        let Ok(guard) = ARTIST_DOC.lock() else {
            return;
        };
        let Some((generation, doc)) = guard.as_ref() else {
            return;
        };
        if doc.get("name").and_then(|v| v.as_str()) != Some(artist_name) {
            return;
        }
        (
            *generation,
            doc.get("artUrl")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        )
    };
    let (generation, artwork_url) = stash;
    // Re-derive the header tint + atmosphere from whatever the portrait is
    // NOW. With the override registered, `cached_path(artwork_url)` already
    // resolves to the custom file; the explicit path arm is for the artists
    // that have no Qobuz portrait to key on.
    let header_art = if path.is_empty() {
        artwork_url
    } else {
        path.clone()
    };
    let published = path.clone();
    publish_patch(generation, move |doc| {
        doc["hasCustomImage"] = json!(!published.is_empty());
        doc["customImageUrl"] = json!(if published.is_empty() {
            String::new()
        } else {
            crate::artwork_qt::file_url(&published)
        });
        doc["customImagePath"] = json!(published);
    });
    spawn_header_color(generation, header_art);
}

/// Resolve the artist portrait (downloading it if needed), reduce it to the
/// header tint (`album_qt::header_tint_hex` — ONE colour pipeline for both
/// detail pages, exactly as the Slint shares `artwork::header_tint`) and
/// patch it in. Generation-guarded: a slow portrait for artist A can never
/// tint artist B's header. Same benign publish-ordering note as
/// `album_qt::spawn_header_color` — the stash is written first, so a
/// clobbered publish is recovered by the next enrichment republish (stories
/// always run).
fn spawn_header_color(generation: u64, artwork_url: String) {
    if artwork_url.is_empty() {
        return;
    }
    crate::spawn(async move {
        if crate::artwork_qt::cached_path(&artwork_url).is_empty() {
            crate::artwork_qt::download_missing(vec![artwork_url.clone()]).await;
        }
        let path = crate::artwork_qt::cached_path(&artwork_url);
        if path.is_empty() {
            return;
        }
        let path = crate::artwork_qt::local_path(&path);
        // One decode, two products: the flat tint (Slint's FALLBACK arm) and
        // the blurred atmosphere it actually renders behind the header.
        let p2 = path.clone();
        let hex = tokio::task::spawn_blocking(move || crate::album_qt::header_tint_hex(&path))
            .await
            .ok()
            .flatten();
        let atmo =
            tokio::task::spawn_blocking(move || crate::atmosphere_qt::for_cover_blocking(&p2))
                .await
                .ok()
                .flatten();
        if hex.is_some() || atmo.is_some() {
            publish_patch(generation, move |doc| {
                if let Some(hex) = hex {
                    doc["headerColor"] = serde_json::json!(hex);
                }
                if let Some(atmo) = atmo {
                    doc["headerAtmosphere"] = serde_json::json!(atmo);
                }
            });
        }
    });
}

/// Patch fields inside the `network` object and re-publish.
fn publish_network(generation: u64, fields: Vec<(&'static str, serde_json::Value)>) {
    publish_patch(generation, move |doc| {
        if let Some(net) = doc.get_mut("network").and_then(|v| v.as_object_mut()) {
            for (key, value) in fields {
                net.insert(key.to_string(), value);
            }
        }
    });
}

/// Every MB section off at once — MB disabled, no confident match, or the
/// resolve failed. The sidebar renders NOTHING for them (opt-in rule).
fn publish_mb_unavailable(generation: u64) {
    publish_network(
        generation,
        vec![
            ("mbAvailable", json!(false)),
            ("originLoading", json!(false)),
            ("relationshipsLoading", json!(false)),
            ("discoveryLoading", json!(false)),
        ],
    );
}

fn spawn_enrichment(
    runtime: Arc<AppRuntime<LoggingAdapter>>,
    generation: u64,
    artist_id: String,
    artist_name: String,
    evidence: SourceEvidence,
    similar_names: Vec<String>,
    mb_on: bool,
) {
    // --- Magazine stories (Qobuz editorial, MB-independent) ---------------
    {
        let runtime = runtime.clone();
        crate::spawn(async move {
            let stories = load_stories(&runtime, &artist_id).await;
            publish_patch(generation, move |doc| {
                doc["stories"] = serde_json::to_value(&stories).unwrap_or_else(|_| json!([]));
                doc["storiesLoading"] = json!(false);
            });
        });
    }

    // --- MusicBrainz network sidebar --------------------------------------
    // Hard gate: with MB off nothing below runs, so nothing leaves the process.
    if !mb_on {
        return;
    }
    crate::spawn(async move {
        let meta = match load_mb_metadata(&runtime, &evidence, &artist_name).await {
            Ok(Some(meta)) => meta,
            Ok(None) => {
                publish_mb_unavailable(generation);
                return;
            }
            Err(e) => {
                log::warn!("[qbz-qt] MB metadata load failed: {e}");
                publish_mb_unavailable(generation);
                return;
            }
        };
        let mbid = meta.mbid.clone();
        publish_network(
            generation,
            vec![
                ("mbid", json!(mbid.clone())),
                (
                    "origin",
                    serde_json::to_value(&meta.origin).unwrap_or_else(|_| json!({})),
                ),
                ("originLoading", json!(false)),
            ],
        );

        match load_mb_relationships(&runtime, &mbid).await {
            Ok(rel) => publish_network(
                generation,
                vec![
                    (
                        "relationships",
                        serde_json::to_value(&rel).unwrap_or_else(|_| json!({})),
                    ),
                    ("relationshipsLoading", json!(false)),
                ],
            ),
            Err(e) => {
                log::warn!("[qbz-qt] MB relationships failed: {e}");
                publish_network(generation, vec![("relationshipsLoading", json!(false))]);
            }
        }

        match load_mb_discovery(&runtime, &mbid, &artist_name, similar_names).await {
            Ok((tag, rows)) => publish_network(
                generation,
                vec![
                    ("discoveryTag", json!(tag)),
                    (
                        "discovery",
                        serde_json::to_value(&rows).unwrap_or_else(|_| json!([])),
                    ),
                    ("discoveryLoading", json!(false)),
                ],
            ),
            Err(e) => {
                log::warn!("[qbz-qt] MB discovery failed: {e}");
                publish_network(generation, vec![("discoveryLoading", json!(false))]);
            }
        }
    });
}

// ----- Magazine stories ---------------------------------------------------

fn map_story(item: ArtistStoryItem) -> StoryRow {
    let author = item
        .authors
        .and_then(|list| list.into_iter().next())
        .map(|a| a.name)
        .unwrap_or_default();
    // `image` is a ready-to-use arc-cdn URL; fall back to the first `images[]`.
    let art_url = item
        .image
        .or_else(|| {
            item.images
                .and_then(|list| list.into_iter().next())
                .map(|img| img.url)
        })
        .unwrap_or_default();
    StoryRow {
        url: format!("https://play.qobuz.com/magazine/story/{}", item.id),
        // Magazine content comes from a CMS: titles carry entities (&amp; …),
        // excerpts may additionally carry markup.
        title: qbz_text_utils::strip_html::decode_html_entities(&item.title),
        author,
        excerpt: item
            .description_short
            .as_deref()
            .map(qbz_text_utils::strip_html::strip_html)
            .unwrap_or_default(),
        art_url,
    }
}

/// The artist's Magazine stories (limit 2, like the official client). Any
/// failure yields an empty list and the tab shows "No stories".
async fn load_stories(runtime: &Arc<AppRuntime<LoggingAdapter>>, artist_id: &str) -> Vec<StoryRow> {
    let Ok(id) = artist_id.parse::<u64>() else {
        return Vec::new();
    };
    match runtime.core().get_artist_story(id, 0, 2).await {
        Ok(resp) => resp.items.into_iter().map(map_story).collect(),
        Err(e) => {
            log::warn!("[qbz-qt] artist story load failed: {e}");
            Vec::new()
        }
    }
}

// ----- MusicBrainz: Origin ------------------------------------------------

struct MbMetadata {
    mbid: String,
    origin: MbOriginJson,
}

/// Resolve the SOURCE artist (Qobuz id + name + own Popular-Track ISRCs) to
/// an MBID, then fetch its metadata. `Ok(None)` = MB disabled, unavailable,
/// or no identity strong enough to show — the caller hides every MB section.
/// Missing Network data beats another band's biography (#768). Identity and
/// voting live in core/`identity`; this only passes evidence and renders.
async fn load_mb_metadata(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    evidence: &SourceEvidence,
    artist_name: &str,
) -> Result<Option<MbMetadata>, String> {
    if !runtime.core().musicbrainz_is_enabled().await {
        return Ok(None);
    }
    let resolution = runtime
        .core()
        .musicbrainz_resolve_source_artist(evidence.qobuz_id, artist_name, &evidence.isrcs)
        .await;
    log::info!(
        "[qbz-qt] artist {} {:?}: MB identity {} ({} isrcs offered, {} MB requests, cache {})",
        evidence.qobuz_id,
        artist_name,
        resolution.identity.label(),
        evidence.isrcs.len(),
        resolution.requests,
        resolution.cache
    );
    let Some(mbid) = resolution
        .identity
        .displayable_mbid()
        .map(str::to_string)
        .filter(|m| !m.is_empty())
    else {
        return Ok(None);
    };
    let meta = runtime
        .core()
        .musicbrainz_get_artist_metadata(&mbid)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Some(MbMetadata {
        origin: map_origin(&meta),
        mbid,
    }))
}

fn map_origin(meta: &qbz_integrations::musicbrainz::ArtistMetadata) -> MbOriginJson {
    use qbz_integrations::musicbrainz::{ArtistType, LocationPrecision};

    let is_person = matches!(meta.artist_type, ArtistType::Person);
    let begin_date = meta
        .life_span
        .as_ref()
        .and_then(|ls| ls.begin.as_deref().map(format_mb_date_short))
        .unwrap_or_default();
    let end_date = meta
        .life_span
        .as_ref()
        .and_then(|ls| ls.end.as_deref().map(format_mb_date_short))
        .unwrap_or_default();
    let location_display = meta
        .location
        .as_ref()
        .map(|loc| loc.display_name.clone())
        .unwrap_or_default();
    let has_data = !begin_date.is_empty() || !end_date.is_empty() || !location_display.is_empty();

    // ---- Artist Scene payload, from the SAME metadata this call already has.
    // No extra fetch: `meta` carries the whole location and the affinity
    // seeds, and every one of these was being discarded here.
    let loc = meta.location.as_ref();
    let opt = |s: &Option<String>| s.clone().unwrap_or_default();
    let location_precision = loc
        .map(|l| match l.precision {
            LocationPrecision::City => "city",
            LocationPrecision::State => "state",
            LocationPrecision::Country => "country",
        })
        .unwrap_or("")
        .to_string();
    // The INTENDED form of Tauri's guard — see the field's doc.
    let location_clickable =
        loc.is_some_and(|l| !matches!(l.precision, LocationPrecision::Country) || l.city.is_some());

    MbOriginJson {
        is_person,
        begin_date,
        end_date,
        location_display,
        has_data,
        location_area_id: loc.map(|l| opt(&l.area_id)).unwrap_or_default(),
        location_city: loc.map(|l| opt(&l.city)).unwrap_or_default(),
        location_country: loc.map(|l| opt(&l.country)).unwrap_or_default(),
        location_country_code: loc.map(|l| opt(&l.country_code)).unwrap_or_default(),
        location_precision,
        seed_genres: meta.affinity_seeds.genres.clone(),
        seed_tags: meta.affinity_seeds.tags.clone(),
        artist_name: meta.name.clone(),
        location_clickable,
    }
}

/// A MusicBrainz partial date as "1990" / "May 1990" / "May 14, 1990"
/// (artist.rs `format_mb_date_short`; month names go through the catalog).
fn format_mb_date_short(date: &str) -> String {
    let parts: Vec<&str> = date.split('-').collect();
    let month = |m: &str| -> Option<&'static str> {
        Some(match m {
            "01" => qbz_i18n::mark("January"),
            "02" => qbz_i18n::mark("February"),
            "03" => qbz_i18n::mark("March"),
            "04" => qbz_i18n::mark("April"),
            "05" => qbz_i18n::mark("May"),
            "06" => qbz_i18n::mark("June"),
            "07" => qbz_i18n::mark("July"),
            "08" => qbz_i18n::mark("August"),
            "09" => qbz_i18n::mark("September"),
            "10" => qbz_i18n::mark("October"),
            "11" => qbz_i18n::mark("November"),
            "12" => qbz_i18n::mark("December"),
            _ => return None,
        })
    };
    match parts.as_slice() {
        [y] => (*y).to_string(),
        [y, m] => match month(m) {
            Some(name) => {
                let name_tr = qbz_i18n::t(name);
                qbz_i18n::t_args("{} {}", &[name_tr.as_str(), *y])
            }
            None => date.to_string(),
        },
        [y, m, d] => match month(m) {
            Some(name) => {
                let day = d.trim_start_matches('0');
                let name_tr = qbz_i18n::t(name);
                qbz_i18n::t_args("{} {}, {}", &[name_tr.as_str(), day, *y])
            }
            None => date.to_string(),
        },
        _ => date.to_string(),
    }
}

// ----- MusicBrainz: Relationships -----------------------------------------

async fn load_mb_relationships(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    mbid: &str,
) -> Result<MbRelationshipsJson, String> {
    let relations = runtime
        .core()
        .musicbrainz_get_artist_relationships(mbid)
        .await
        .map_err(|e| e.to_string())?;
    Ok(map_relationships(relations))
}

fn map_relationships(
    rels: qbz_integrations::musicbrainz::ArtistRelationships,
) -> MbRelationshipsJson {
    let members = group_relations(rels.members, "Band Member");
    let groups = group_relations(rels.groups, "Band");
    let collaborators = group_relations(rels.collaborators, "Collaborator");
    let has_data = !members.is_empty() || !groups.is_empty() || !collaborators.is_empty();
    MbRelationshipsJson {
        members,
        groups,
        collaborators,
        has_data,
    }
}

/// Group repeated relations by mbid, combining their roles (artist.rs
/// `group_relations` / Tauri's `groupMembersByMbid`).
fn group_relations(
    rels: Vec<qbz_integrations::musicbrainz::RelatedArtist>,
    default_role: &str,
) -> Vec<MbRelationshipJson> {
    struct Pending {
        name: String,
        roles: Vec<String>,
        begin: Option<String>,
        end: Option<String>,
    }
    let mut by_mbid: HashMap<String, Pending> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for r in rels {
        let begin = r.period.as_ref().and_then(|p| p.begin.clone());
        let end = r.period.as_ref().and_then(|p| p.end.clone());
        match by_mbid.get_mut(&r.mbid) {
            Some(existing) => {
                if let Some(role) = r.role.clone() {
                    if !existing.roles.iter().any(|rr| rr == &role) {
                        existing.roles.push(role);
                    }
                }
            }
            None => {
                order.push(r.mbid.clone());
                let mut roles = Vec::new();
                if let Some(role) = r.role.clone() {
                    roles.push(role);
                }
                by_mbid.insert(
                    r.mbid.clone(),
                    Pending {
                        name: r.name,
                        roles,
                        begin,
                        end,
                    },
                );
            }
        }
    }
    order
        .into_iter()
        .filter_map(|mbid| by_mbid.remove(&mbid).map(|p| (mbid, p)))
        .map(|(mbid, p)| {
            let period = format_period(p.begin.as_deref(), p.end.as_deref());
            let tooltip = if !p.roles.is_empty() {
                let roles_joined = p.roles.join(", ");
                if period.is_empty() {
                    roles_joined
                } else {
                    format!("{roles_joined} ({period})")
                }
            } else if !period.is_empty() {
                period.clone()
            } else {
                p.name.clone()
            };
            let role = p
                .roles
                .first()
                .cloned()
                .unwrap_or_else(|| default_role.to_string());
            MbRelationshipJson {
                mbid,
                name: p.name,
                role,
                tooltip,
            }
        })
        .collect()
}

fn format_period(begin: Option<&str>, end: Option<&str>) -> String {
    if begin.is_some() || end.is_some() {
        format!("{} - {}", begin.unwrap_or("?"), end.unwrap_or("present"))
    } else {
        String::new()
    }
}

// ----- MusicBrainz: Discovery ("You may also like") -----------------------

/// Tag-seeded discovery candidates, validated against Qobuz by the core.
/// Returns `(primary_tag, rows)`.
///
/// The `dismissed_per_tag` / `known_artists` callbacks are EMPTY here: the
/// Slint app feeds them from its `discovery_dismiss`, `play_history` and
/// `reco` stores; this path does not consult them yet. Empty = no exclusion, which
/// is exactly what a first-run Slint profile does.
async fn load_mb_discovery(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    seed_mbid: &str,
    seed_name: &str,
    similar_names: Vec<String>,
) -> Result<(String, Vec<MbDiscoveryJson>), String> {
    let dismissed = |_tag: &str| HashSet::<String>::new();
    let known = || (HashSet::<u64>::new(), HashSet::<String>::new());
    let response = runtime
        .core()
        .musicbrainz_discover_artists(seed_mbid, seed_name, &similar_names, &dismissed, &known)
        .await
        .map_err(|e| e.to_string())?;
    let rows = response
        .artists
        .into_iter()
        .map(|a| MbDiscoveryJson {
            mbid: a.mbid,
            name: a.name,
            qobuz_id: a.qobuz_id.map(|id| id.to_string()).unwrap_or_default(),
        })
        .collect();
    Ok((response.primary_tag, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE #737 CONTRACT: `top_queue` answers for the POPULAR list only. An
    /// id it does not hold does not fail — it starts that list at row 0. So
    /// no caller may hand it an id from any other list (the "In library" tab
    /// routes through `library_qt::play_from_visible` instead).
    #[test]
    fn top_queue_starts_at_zero_for_a_foreign_id() {
        TOP_QUEUE.lock().unwrap().clear();
        let (queue, start) = top_queue(Some(424242));
        assert!(queue.is_empty());
        assert_eq!(start, 0);
    }
}

#[cfg(test)]
mod release_date_sort_regression {
    #[test]
    fn popularity_requests_invalidate_old_pages_but_local_sorts_keep_pagination() {
        let mut section = serde_json::json!({"sortBy":"default", "sortRevision":0});
        let initial = super::section_request(7, &section);
        assert_eq!(initial.order, "release_date");
        section["sortBy"] = serde_json::json!("newest");
        assert_eq!(initial, super::section_request(7, &section));
        section["sortBy"] = serde_json::json!("relevant");
        section["sortRevision"] = serde_json::json!(1);
        let popularity = super::section_request(7, &section);
        assert_eq!(popularity.order, "relevant");
        assert_ne!(initial, popularity);
        section["sortBy"] = serde_json::json!("default");
        section["sortRevision"] = serde_json::json!(2);
        assert_ne!(
            initial,
            super::section_request(7, &section),
            "late date page from before popularity must drop"
        );
        assert_ne!(
            super::section_request(7, &section),
            super::section_request(8, &section)
        );
    }

    use super::*;

    #[test]
    fn artist_cards_use_original_dates_and_preserve_default_order() {
        let mut cards: Vec<_> = [
            ("souls", "2015-09-04"),
            ("iron", "1980-04-01"),
            ("senjutsu", "2021-09-03"),
            ("live", "1985-08-01"),
        ]
        .into_iter()
        .map(|(id, original)| {
            let release: PageArtistRelease = serde_json::from_value(serde_json::json!({
                "id": id, "title": id, "dates": {"original": original, "stream": "2026-09-08"}
            }))
            .unwrap();
            map_release(&release)
        })
        .collect();
        for (index, card) in cards.iter_mut().enumerate() {
            card.default_order = index as u32;
        }
        let ids = |cards: &[AlbumCardData]| cards.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        sort_release_cards(&mut cards, "default");
        assert_eq!(ids(&cards), ["souls", "iron", "senjutsu", "live"]);
        sort_release_cards(&mut cards, "oldest");
        assert_eq!(ids(&cards), ["iron", "live", "souls", "senjutsu"]);
        sort_release_cards(&mut cards, "newest");
        assert_eq!(ids(&cards), ["senjutsu", "souls", "live", "iron"]);
        let json = serde_json::to_value(&cards).unwrap();
        assert_eq!(json[0]["releaseSortKey"], 20210903);
        assert!(!cards[0].year.is_empty());
        let mut json_cards: Vec<serde_json::Value> = cards
            .iter()
            .map(|c| serde_json::to_value(c).unwrap())
            .collect();
        sort_release_values(&mut json_cards, "default");
        assert_eq!(
            json_cards
                .iter()
                .map(|c| c["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["souls", "iron", "senjutsu", "live"]
        );
    }
}
