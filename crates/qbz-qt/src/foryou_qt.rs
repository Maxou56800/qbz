//! Discover > For You — the three rails that are NOT plain album carousels,
//! plus the Qobuz mix landing page they lead to.
//!
//! Ported from the Slint build, which spreads the same feature over four
//! files:
//!   - `discover/QobuzMixesRow.slint`  — the four static navigation tiles
//!     (already in `HomeView.qml`; this module supplies what they OPEN).
//!   - `discover/RadioCarousel.slint` + `RadioCard.slint` — album-seeded
//!     radio tiles, built by `foryou.rs::build_radio`.
//!   - `discover/Spotlight.slint` — one favourite artist, built by
//!     `foryou.rs::load_spotlight`.
//!   - `mix/MixView.slint` + `crates/qbz/src/mix.rs` — the DailyQ / WeeklyQ /
//!     FavQ / TopQ detail page.
//!
//! ## Transport
//!
//! Radio + Spotlight ride their OWN bridge properties (`radioStationsJson` /
//! `spotlightJson`), NOT the three tab documents — the `pinned` precedent
//! (`home_qt::publish_pinned`). The tab documents carry an EMPTY ordering
//! slot for each (`kind: "radio"` / `"spotlight"`), which is where the
//! discover prefs place the rail; the rows arrive separately so the Spotlight
//! landing (one extra `/artist/page` round trip) never republishes — and so
//! never re-creates the delegates of — every other rail on all three tabs.
//! HomeView self-hides both slots while their document is empty, 1:1 with the
//! Slint `ForYouState.radio-stations.length > 0` / `spotlight-visible` gates.
//!
//! The mix page is ONE document (`mixJson`), the `label_qt` shape: header
//! fields + `tracks[]` in the `qml/rows/TrackRow.qml` item contract, with a
//! generation counter so a stale fetch cannot overwrite a newer page.
//!
//! ## Shared crates reused (no new client code)
//!
//! | need | shared call |
//! |---|---|
//! | DailyQ / WeeklyQ | `core().get_dynamic_suggest_full()` + `get_dynamic_suggest()` fallback |
//! | seed resolution  | `core().get_track()` (the `track_to_analysed` payload) |
//! | FavQ             | `core().get_favorites("tracks", 200, 0)` |
//! | TopQ             | `core().get_user_playlists()` + `get_playlist()` |
//! | Spotlight        | `core().get_artist_page()` (+ `artist_qt::map_release` shape) |
//! | album radio      | `core().get_radio_album()` + `get_tracks_batch()` enrich |
//! | artist radio     | `core().create_smart_artist_radio()` (the `qbz-radio` pool builder) |
//! | qobuz artist radio | `core().get_radio_artist()` + `get_tracks_batch()` enrich |
//! | artist top tracks| `playback_qt::play_artist()` |
//!
//! ## POC-NOTEs (deliberate deltas vs the Slint build)
//! - `mix_listened_seed_ids` prefers the reco store's scored seeds in Slint
//!   (`crate::reco::home_seeds`) and falls back to recents + favourites when
//!   the store is cold. This port has no `qbz-reco` dependency, so it is the
//!   FALLBACK path only — the same delta `home_qt` already documents for
//!   "Rediscover Your Library".
//! - The shared artist blacklist store is live, but mix-row mapping does not
//!   yet consult it, so the rows carry no `isBlacklisted` grey-out.
//! - The mix page has no multi-select bar: `qml/rows/TrackRow.qml` has no
//!   multi-select arm and two of the Slint bar's seven actions have no bridge
//!   seam at all — the same call `LabelView.qml` already documents. The
//!   Slint header's select disc renders DIMMED and inert here (the ArtistView
//!   / LabelView precedent), and "Add to playlist" (`list-plus`) is likewise
//!   inert, exactly as in the Slint header where it carries no `clicked`.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use cxx_qt_lib::QString;
use qbz_app::shell::AppRuntime;
use qbz_core::{FrontendAdapter, LoggingAdapter};
use qbz_models::{Album, Artist, QueueTrack, Track, TrackToAnalyse};
use serde::Serialize;
use serde_json::Value;

use crate::album_qt::TrackRow;
use crate::home_qt::{attach_card_art, HomeCard};

// ===========================================================================
//  Radio Stations rail (RadioCarousel.slint + foryou.rs build_radio)
// ===========================================================================

/// One album-seeded radio tile (`state.slint` `RadioStationItem`).
#[derive(Clone, Default, Serialize)]
pub struct RadioSeedRow {
    #[serde(rename = "albumId")]
    pub album_id: String,
    pub title: String,
    pub artist: String,
    #[serde(rename = "artUrl")]
    pub art_url: String,
    #[serde(rename = "artPath")]
    pub art_path: String,
}

static RADIO: Mutex<Vec<RadioSeedRow>> = Mutex::new(Vec::new());

/// Radio Stations — album-seeded tiles from recent + favourite albums,
/// deduped, capped at 12 (`foryou.rs::build_radio`, verbatim including the
/// Qobuz-source gate: `/radio/album` cannot resolve a local or Plex album id,
/// and an empty source is a legacy entry treated as Qobuz).
pub(crate) fn build_radio(
    recent_albums: &[crate::recently_qt::RecentAlbum],
    fav_albums: &[Album],
) -> Vec<RadioSeedRow> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<RadioSeedRow> = Vec::new();
    for a in recent_albums {
        if !(a.source.is_empty() || a.source.eq_ignore_ascii_case("qobuz")) {
            continue;
        }
        if seen.insert(a.id.clone()) {
            out.push(RadioSeedRow {
                album_id: a.id.clone(),
                title: a.title.clone(),
                artist: a.artist.clone(),
                art_url: a.artwork_url.clone(),
                art_path: String::new(),
            });
        }
    }
    for a in fav_albums {
        if out.len() >= 12 {
            break;
        }
        if seen.insert(a.id.clone()) {
            out.push(RadioSeedRow {
                album_id: a.id.clone(),
                title: a.title.clone(),
                artist: a.artist.name.clone(),
                // Grid card (RadioCard on the For You rail): full variant
                // (best()) — the down-tier was reverted after the 2026-08-15
                // owner smoke (contract 04 §3).
                art_url: a.image.best().cloned().unwrap_or_default(),
                art_path: String::new(),
            });
        }
    }
    out.truncate(12);
    out
}

/// The landed covers of ONE batch as a `{artUrl: file://path}` patch, instead
/// of a second full document publish.
///
/// Two separate decisions live here.
///
/// (a) PER URL, NOT PER DOCUMENT. The two-pass `label_qt` shape republished the
/// whole JSON when the covers arrived, which hands QML's `model:` a NEW JS
/// array; `QQuickItemView::setModel()` then resets `contentX` to 0 and tears
/// down the QQmlDelegateModel (this build's only crash signature — `main.rs`
/// documents it for the Library feed, and `LibraryView.qml` documents the cure:
/// patch in place, never re-emit the document).
///
/// (b) ONE EMIT PER BATCH, NOT ONE PER URL. `HomeView.qml`'s `forYouArt` is a
/// `var` property, so only a NEW object reference notifies its dependents: an
/// emit per url paid one copy of the (growing) map AND one invalidation sweep
/// over every `forYouArtOf(...)` binding in both rails plus the `artPending`
/// probe — quadratic in the ~38 covers a cold For You load resolves, for 38 Qt
/// thread hops. Nothing is shown any later for it: `download_missing().await`
/// settles the WHOLE batch before the first path can be read back.
///
/// The emit shape is the port's existing signal helper (`emit_library_favorite`
/// in main.rs): a cxx-qt signal is emitted by calling the declared Rust method
/// on `Pin<&mut T>`.
fn emit_foryou_art(patch: BTreeMap<String, String>) {
    if patch.is_empty() {
        return;
    }
    let json = patch_json(&patch);
    crate::home_bridge::ui(move |mut b| {
        b.as_mut().foryou_art_ready(QString::from(json.as_str()));
    });
}

fn patch_json(patch: &BTreeMap<String, String>) -> String {
    serde_json::to_string(patch).unwrap_or_else(|_| "{}".to_string())
}

/// Collect one resolved cover into the patch (blanks are not patches).
fn collect_resolved(patch: &mut BTreeMap<String, String>, url: &str, path: &str) {
    if !url.is_empty() && !path.is_empty() {
        patch.insert(url.to_string(), path.to_string());
    }
}

/// Every For You cover the stores have ALREADY resolved, as the same
/// `{artUrl: file://path}` patch [`emit_foryou_art`] sends — or "" when there
/// is nothing to hand over yet.
///
/// WHY THIS EXISTS. `AppShell.qml` binds `viewLoader.source` to
/// `QbzShell.currentView`, so HomeView is DESTROYED on every navigation and its
/// per-url `forYouArt` map dies with it, while the rails' documents are
/// deliberately never republished for artwork (see [`emit_foryou_art`]) and
/// `publish_radio` / `publish_spotlight` run ONCE per session (`home_qt::
/// load_home` <- `reload_home` <- the boot chain in main.rs; otherwise only the
/// error-retry button and a language change). Without a re-hand on mount the
/// Radio rail and the Spotlight hero + album tiles come back BLANK for the rest
/// of the session — and `anyForYouArtPending` stays true forever, which pins
/// HomeView's 900 ms skeleton pulse Timer permanently ON, i.e. it also defeats
/// the perf fix this transport exists for.
///
/// It re-resolves the cache paths into the stores first, so a cover that landed
/// while Home was unmounted (that emit reached nobody) — or that some other
/// view downloaded — is picked up. `cached_path` is memoized, so the second and
/// later calls touch neither SQLite nor the disk.
///
/// It NEVER downloads and NEVER republishes a document, and it returns "" when
/// nothing is resolved, so the FIRST (cold) mount emits nothing at all and
/// cannot clear a map the in-flight download is about to fill.
pub(crate) fn resolved_art_patch() -> String {
    let mut patch: BTreeMap<String, String> = BTreeMap::new();
    if let Ok(mut store) = RADIO.lock() {
        let _ = attach_radio_art(&mut store);
        for row in store.iter() {
            collect_resolved(&mut patch, &row.art_url, &row.art_path);
        }
    }
    if let Ok(mut store) = SPOTLIGHT.lock() {
        if let Some(doc) = store.as_mut() {
            let _ = attach_spotlight_art(doc);
            collect_resolved(&mut patch, &doc.art_url, &doc.art_path);
            for card in doc.albums.iter() {
                collect_resolved(&mut patch, &card.art_url, &card.art_path);
            }
        }
    }
    if patch.is_empty() {
        return String::new();
    }
    log::debug!(
        "[qbz-qt] for-you art re-handed to a fresh HomeView: {} covers",
        patch.len()
    );
    patch_json(&patch)
}

/// Store + publish the radio rail ONCE, then fetch the covers it is missing
/// and hand them to the rail as a url-keyed patch (see [`emit_foryou_art`]).
pub(crate) fn publish_radio(rows: Vec<RadioSeedRow>) {
    let missing = {
        let Ok(mut store) = RADIO.lock() else {
            return;
        };
        *store = rows;
        attach_radio_art(&mut store)
    };
    push_radio();
    if missing.is_empty() {
        return;
    }
    crate::spawn(async move {
        crate::artwork_qt::download_missing(missing.clone()).await;
        // In place: the rows keep the art they already have and the landed
        // covers reach the rail as one url-keyed patch. The store is still
        // updated so a LATER full publish (a new Home load) carries the
        // paths — and so `resolved_art_patch` can re-hand them to a HomeView
        // that was destroyed and re-created meanwhile — but nothing
        // republishes for artwork alone.
        if let Ok(mut store) = RADIO.lock() {
            let _ = attach_radio_art(&mut store);
        }
        let mut patch: BTreeMap<String, String> = BTreeMap::new();
        for url in missing {
            let path = crate::artwork_qt::cached_path(&url);
            collect_resolved(&mut patch, &url, &path);
        }
        emit_foryou_art(patch);
    });
}

fn attach_radio_art(rows: &mut [RadioSeedRow]) -> Vec<String> {
    let mut missing = Vec::new();
    for row in rows.iter_mut() {
        if row.art_url.is_empty() {
            continue;
        }
        let hit = crate::artwork_qt::cached_path(&row.art_url);
        if hit.is_empty() {
            missing.push(row.art_url.clone());
        } else {
            row.art_path = hit;
        }
    }
    missing.sort();
    missing.dedup();
    missing
}

fn push_radio() {
    let json = RADIO
        .lock()
        .ok()
        .map(|rows| serde_json::to_string(&*rows).unwrap_or_else(|_| "[]".to_string()))
        .unwrap_or_else(|| "[]".to_string());
    crate::home_bridge::ui(move |mut b| {
        b.as_mut()
            .set_radio_stations_json(QString::from(json.as_str()));
    });
}

// ===========================================================================
//  Spotlight (Spotlight.slint + foryou.rs load_spotlight)
// ===========================================================================

#[derive(Clone, Default, Serialize)]
pub struct SpotlightDoc {
    pub visible: bool,
    #[serde(rename = "artistId")]
    pub artist_id: String,
    pub name: String,
    pub category: String,
    #[serde(rename = "artUrl")]
    pub art_url: String,
    #[serde(rename = "artPath")]
    pub art_path: String,
    #[serde(rename = "hasTopTracks")]
    pub has_top_tracks: bool,
    pub albums: Vec<HomeCard>,
}

static SPOTLIGHT: Mutex<Option<SpotlightDoc>> = Mutex::new(None);

/// One favourite artist highlighted (`foryou.rs::load_spotlight`): rotate
/// among the top 5 favourites by wall-clock seconds, fetch that artist's
/// page, take up to 6 releases preferring `album` then `live` / `ep-single` /
/// `compilation` in that order.
pub(crate) fn spawn_spotlight<A>(runtime: Arc<AppRuntime<A>>, favorites: Vec<Artist>)
where
    A: FrontendAdapter + Send + Sync + 'static,
{
    crate::spawn(async move {
        let doc = load_spotlight(&runtime, &favorites).await;
        publish_spotlight(doc);
    });
}

async fn load_spotlight<A>(
    runtime: &Arc<AppRuntime<A>>,
    favorites: &[Artist],
) -> Option<SpotlightDoc>
where
    A: FrontendAdapter + Send + Sync + 'static,
{
    if favorites.is_empty() {
        return None;
    }
    let pool = favorites.len().min(5);
    let idx = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as usize % pool)
        .unwrap_or(0);
    let seed = &favorites[idx];

    let page = match runtime.core().get_artist_page(seed.id, None).await {
        Ok(page) => page,
        Err(e) => {
            log::warn!("[qbz-qt] spotlight artist page {} failed: {e}", seed.id);
            return None;
        }
    };
    let art_url = page
        .images
        .as_ref()
        .and_then(|i| i.portrait.as_ref())
        .map(|p| {
            format!(
                "https://static.qobuz.com/images/artists/covers/medium/{}.{}",
                p.hash, p.format
            )
        })
        .unwrap_or_default();

    let mut seen: HashSet<String> = HashSet::new();
    let mut albums: Vec<HomeCard> = Vec::new();
    for want in ["album", "live", "ep-single", "compilation"] {
        if albums.len() >= 6 {
            break;
        }
        let Some(groups) = page.releases.as_ref() else {
            break;
        };
        let Some(group) = groups.iter().find(|g| g.release_type == want) else {
            continue;
        };
        for rel in &group.items {
            if !seen.insert(rel.id.clone()) {
                continue;
            }
            // `artist_qt::map_release` is the shared artist-page release
            // mapper (rule 5: reuse over a second copy); only the two fields
            // the rail transport adds on top are filled in here.
            let card = crate::artist_qt::map_release(rel);
            albums.push(HomeCard {
                is_pinned: card.is_pinned,
                is_favorite: card.is_favorite,
                id: card.id,
                title: card.title,
                artist: if card.artist.is_empty() {
                    page.name.display.clone()
                } else {
                    card.artist
                },
                artist_id: card.artist_id,
                genre: card.genre,
                year: card.year,
                quality_tier: card.quality_tier,
                quality_detail: card.quality_detail,
                art_url: card.art_url,
                ..HomeCard::default()
            });
            if albums.len() >= 6 {
                break;
            }
        }
    }

    Some(SpotlightDoc {
        visible: true,
        artist_id: seed.id.to_string(),
        name: page.name.display.clone(),
        category: page.artist_category.clone().unwrap_or_default(),
        art_url,
        art_path: String::new(),
        has_top_tracks: page
            .top_tracks
            .as_ref()
            .map(|t| !t.is_empty())
            .unwrap_or(false),
        albums,
    })
}

fn publish_spotlight(doc: Option<SpotlightDoc>) {
    let missing = {
        let Ok(mut store) = SPOTLIGHT.lock() else {
            return;
        };
        *store = doc;
        store.as_mut().map(attach_spotlight_art).unwrap_or_default()
    };
    push_spotlight();
    if missing.is_empty() {
        return;
    }
    crate::spawn(async move {
        crate::artwork_qt::download_missing(missing.clone()).await;
        // Same in-place patch as the radio rail: the hero portrait and every
        // album tile arrive inside ONE `foryouArtReady`, so the Spotlight
        // document is never republished for artwork alone.
        if let Ok(mut store) = SPOTLIGHT.lock() {
            if let Some(doc) = store.as_mut() {
                let _ = attach_spotlight_art(doc);
            }
        }
        let mut patch: BTreeMap<String, String> = BTreeMap::new();
        for url in missing {
            let path = crate::artwork_qt::cached_path(&url);
            collect_resolved(&mut patch, &url, &path);
        }
        emit_foryou_art(patch);
    });
}

/// THE RULE (label_qt's `attach_doc_art`): every art-bearing list on the
/// document has to be listed here or its tiles stay blank forever.
fn attach_spotlight_art(doc: &mut SpotlightDoc) -> Vec<String> {
    let mut missing = attach_card_art(&mut doc.albums);
    if !doc.art_url.is_empty() {
        let hit = crate::artwork_qt::cached_path(&doc.art_url);
        if hit.is_empty() {
            missing.push(doc.art_url.clone());
        } else {
            doc.art_path = hit;
        }
    }
    missing.sort();
    missing.dedup();
    missing
}

fn push_spotlight() {
    let json = SPOTLIGHT
        .lock()
        .ok()
        .and_then(|doc| doc.as_ref().and_then(|d| serde_json::to_string(d).ok()))
        .unwrap_or_else(|| "{}".to_string());
    crate::home_bridge::ui(move |mut b| {
        b.as_mut().set_spotlight_json(QString::from(json.as_str()));
    });
}

/// Spotlight "play top tracks" (the primary circle button and the TOP TRACKS
/// card) — `media-action("artist", id, "play-top")` in Slint, which resolves
/// to `playback::play_artist_top_tracks`. `playback_qt::play_artist` is the
/// port of that path (top tracks, studio discography as the fallback).
pub(crate) fn spotlight_play_top_tracks(artist_id: String) {
    let runtime = crate::app();
    crate::spawn(async move {
        if let Err(e) = crate::playback_qt::play_artist(&runtime, &artist_id).await {
            log::warn!("[qbz-qt] spotlight top tracks {artist_id} failed: {e}");
        }
    });
}

// ---------------------------------------------------------------------------
//  Radio start-up indicator (QbzHome.radioPending)
// ---------------------------------------------------------------------------

/// Publish "a radio is being built for this key" / "nothing is".
///
/// EVERY radio entry point below arms this on the way in and disarms it on
/// EVERY way out — the success path, the empty-result path and the error path
/// alike. A leaked key is worse than no indicator at all: it is a disc that
/// spins forever on a page the user is still looking at, and the only two
/// exits from a network call are "it worked" and "it did not".
///
/// `RadioGuard` is what makes that structural rather than a rule three
/// functions have to remember: it disarms in `Drop`, so an early `return` — or
/// a future cancelled before it finishes — cannot skip it.
fn set_radio_pending(key: &str) {
    crate::home_bridge::publish_radio_pending(key.to_string());
}

/// Armed for the life of one radio build; disarms itself however the build
/// ends. Held across the `.await`s, so it is dropped on the same task.
struct RadioGuard;

impl RadioGuard {
    fn arm(key: &str) -> Self {
        set_radio_pending(key);
        RadioGuard
    }
}

impl Drop for RadioGuard {
    fn drop(&mut self) {
        set_radio_pending("");
    }
}

/// Spotlight RADIO card and the artist page's "QBZ Radio" row —
/// `media-action("artist", id, "radio")` / `"radio-qbz"`, i.e. the SMART pool
/// builder (`qbz-radio`), not `/radio/artist`.
pub(crate) fn start_artist_radio(artist_id: String) {
    let runtime = crate::app();
    crate::spawn(async move {
        // Armed BEFORE the parse: a bad id returns immediately and the guard's
        // Drop clears it in the same turn, so the indicator never flickers,
        // but the arming does not have to be duplicated after every early exit.
        let _pending = RadioGuard::arm(&format!("artist:{artist_id}"));
        let Ok(id) = artist_id.parse::<u64>() else {
            log::warn!("[qbz-qt] smart radio: bad artist id {artist_id}");
            return;
        };
        match runtime.core().create_smart_artist_radio(id).await {
            Ok(tracks) if !tracks.is_empty() => play_flat(&runtime, tracks, 0, false).await,
            Ok(_) => log::warn!("[qbz-qt] smart artist radio {id} returned no tracks"),
            Err(e) => log::warn!("[qbz-qt] smart artist radio {id} failed: {e}"),
        }
    });
}

/// The "Qobuz Radio" arm of the ARTIST PAGE's radio dropdown —
/// `media-action("artist", id, "radio-qobuz")` in Slint
/// (`ArtistPageView.slint:457` -> `main.rs:12906` ->
/// `playback::play_artist_radio`). The plain Qobuz `/radio/artist` endpoint,
/// as opposed to `start_artist_radio` above, which is the SMART `qbz-radio`
/// pool builder wired to the dropdown's other row.
///
/// Its track objects are MINIMAL, exactly like the album radio's, so they go
/// through the same `enrich_radio_tracks` re-fetch — without it the queue rows
/// show no artist.
pub(crate) fn start_qobuz_artist_radio(artist_id: String) {
    let runtime = crate::app();
    crate::spawn(async move {
        // Same key as the smart arm: they are two rows of ONE dropdown on ONE
        // disc, and that disc is what spins.
        let _pending = RadioGuard::arm(&format!("artist:{artist_id}"));
        match runtime.core().get_radio_artist(&artist_id).await {
            Ok(resp) => {
                let tracks = enrich_radio_tracks(&runtime, resp.tracks.items).await;
                if tracks.is_empty() {
                    log::warn!("[qbz-qt] qobuz artist radio {artist_id} returned no tracks");
                    return;
                }
                play_flat(&runtime, tracks, 0, false).await;
            }
            Err(e) => log::warn!("[qbz-qt] qobuz artist radio {artist_id} failed: {e}"),
        }
    });
}

/// Radio Stations tile — `media-action("album", id, "radio")`, the Qobuz
/// `/radio/album` endpoint. Its track objects are MINIMAL (no performer, no
/// album), so they are re-fetched by id exactly like `playback.rs::
/// enrich_radio_tracks` or the queue rows would show no artist.
pub(crate) fn start_album_radio(album_id: String) {
    let runtime = crate::app();
    crate::spawn(async move {
        let _pending = RadioGuard::arm(&format!("album:{album_id}"));
        match runtime.core().get_radio_album(&album_id).await {
            Ok(resp) => {
                let tracks = enrich_radio_tracks(&runtime, resp.tracks.items).await;
                if tracks.is_empty() {
                    log::warn!("[qbz-qt] album radio {album_id} returned no tracks");
                    return;
                }
                play_flat(&runtime, tracks, 0, false).await;
            }
            Err(e) => log::warn!("[qbz-qt] album radio {album_id} failed: {e}"),
        }
    });
}

async fn enrich_radio_tracks(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    tracks: Vec<Track>,
) -> Vec<Track> {
    let ids: Vec<u64> = tracks.iter().map(|t| t.id).collect();
    if ids.is_empty() {
        return tracks;
    }
    match runtime.core().get_tracks_batch(&ids).await {
        Ok(full) if !full.is_empty() => full,
        _ => tracks,
    }
}

// ===========================================================================
//  Qobuz mix landing page (mix.rs + MixView.slint)
// ===========================================================================

/// Title + description per mix kind (`mix.rs::mix_meta`). NOTE these are NOT
/// the four tile descriptions in `QobuzMixesRow.slint` — the Slint build uses
/// a different string for `weekly` and `top` on the detail page, and this is a
/// 1:1 port of the detail page's.
pub(crate) fn mix_meta(kind: &str) -> (&'static str, String) {
    match kind {
        "daily" => (
            "DailyQ",
            qbz_i18n::t("Elevate your day with a customized selection of music."),
        ),
        "weekly" => ("WeeklyQ", qbz_i18n::t("A fresh mix every week.")),
        "fav" => (
            "FavQ",
            qbz_i18n::t("A fresh shuffle from your personal library."),
        ),
        "top" => ("TopQ", qbz_i18n::t("From your most-played playlists.")),
        _ => ("Mix", String::new()),
    }
}

#[derive(Clone, Default, Serialize)]
pub struct MixDoc {
    /// "daily" | "weekly" | "fav" | "top" — the QML picks the header gradient
    /// off this, matching the tile it was opened from.
    pub kind: String,
    pub title: String,
    pub subtitle: String,
    #[serde(rename = "trackCount")]
    pub track_count: i32,
    #[serde(rename = "totalDuration")]
    pub total_duration: String,
    pub loading: bool,
    /// `qml/rows/TrackRow.qml` item objects (album_qt::TrackRow + artPath).
    pub tracks: Vec<Value>,
}

#[derive(Default)]
struct MixState {
    generation: u64,
    doc: MixDoc,
    /// The resolved tracks, kept so play-all / shuffle / play-from-here build
    /// the queue without re-fetching (`mix.rs::CURRENT_MIX`).
    queue: Vec<QueueTrack>,
}

static MIX: Mutex<Option<MixState>> = Mutex::new(None);

fn with_mix<R>(f: impl FnOnce(&mut MixState) -> R) -> Option<R> {
    let mut guard = MIX.lock().ok()?;
    Some(f(guard.get_or_insert_with(MixState::default)))
}

fn publish_mix() {
    let json = MIX
        .lock()
        .ok()
        .map(|s| match s.as_ref() {
            Some(state) => serde_json::to_string(&state.doc).unwrap_or_else(|_| "{}".to_string()),
            None => "{}".to_string(),
        })
        .unwrap_or_else(|| "{}".to_string());
    crate::home_bridge::ui(move |mut b| {
        b.as_mut().set_mix_json(QString::from(json.as_str()));
    });
}

/// Open a Qobuz mix detail page. Same order as `label_qt::open_label`:
/// offline gate -> `nav_qt::record` (which already publishes `currentView`)
/// -> reset the document + raise `loading` -> fetch.
///
/// The RESET is the `mix.rs::reset_mix` hop: the title / subtitle / gradient
/// are known from the kind alone, so the page paints its header immediately
/// and only the list is pending — opening WeeklyQ after DailyQ never shows
/// DailyQ's header.
pub(crate) fn open_mix(kind: String) {
    if crate::offline_fwd::engine().status().is_offline() {
        return;
    }
    let kind = match kind.as_str() {
        "daily" | "weekly" | "fav" | "top" => kind,
        other => {
            log::warn!("[qbz-qt] open_mix: unknown mix kind {other}");
            return;
        }
    };
    crate::nav_qt::record("mix");
    let (title, subtitle) = mix_meta(&kind);
    let generation = with_mix(|s| {
        s.generation = s.generation.wrapping_add(1);
        s.doc = MixDoc {
            kind: kind.clone(),
            title: title.to_string(),
            subtitle,
            loading: true,
            ..MixDoc::default()
        };
        s.queue.clear();
        s.generation
    });
    let Some(generation) = generation else {
        return;
    };
    publish_mix();
    fetch_mix(generation, kind);
}

/// The header's refresh disc — re-load the mix that is on screen.
pub(crate) fn refresh_mix() {
    let kind = MIX
        .lock()
        .ok()
        .and_then(|s| s.as_ref().map(|state| state.doc.kind.clone()))
        .unwrap_or_default();
    if kind.is_empty() {
        return;
    }
    let generation = with_mix(|s| {
        s.generation = s.generation.wrapping_add(1);
        s.doc.loading = true;
        s.doc.tracks.clear();
        s.doc.track_count = 0;
        s.doc.total_duration = String::new();
        s.queue.clear();
        s.generation
    });
    let Some(generation) = generation else {
        return;
    };
    publish_mix();
    fetch_mix(generation, kind);
}

fn fetch_mix(generation: u64, kind: String) {
    let runtime = crate::app();
    crate::spawn(async move {
        let tracks = load_mix(&runtime, &kind).await;
        let rows: Vec<TrackRow> = tracks.iter().map(to_row).collect();
        let queue: Vec<QueueTrack> = tracks.iter().map(to_queue_track).collect();
        let count = rows.len() as i32;
        let duration = total_duration(&tracks);

        let stale = with_mix(|s| {
            if s.generation != generation {
                return true;
            }
            s.doc.tracks = rows.iter().map(track_row_value).collect();
            s.doc.track_count = count;
            s.doc.total_duration = duration;
            s.doc.loading = false;
            s.queue = queue;
            false
        })
        .unwrap_or(true);
        if stale {
            return;
        }
        let missing = with_mix(attach_mix_art).unwrap_or_default();
        publish_mix();
        if missing.is_empty() {
            return;
        }
        crate::artwork_qt::download_missing(missing).await;
        let stale = with_mix(|s| {
            if s.generation != generation {
                return true;
            }
            let _ = attach_mix_art(s);
            false
        })
        .unwrap_or(true);
        if !stale {
            publish_mix();
        }
    });
}

fn attach_mix_art(state: &mut MixState) -> Vec<String> {
    let mut missing = Vec::new();
    for row in state.doc.tracks.iter_mut() {
        let url = row
            .get("artUrl")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if url.is_empty() {
            continue;
        }
        let hit = crate::artwork_qt::cached_path(&url);
        if hit.is_empty() {
            missing.push(url);
        } else if let Some(obj) = row.as_object_mut() {
            obj.insert("artPath".to_string(), Value::String(hit));
        }
    }
    missing.sort();
    missing.dedup();
    missing
}

/// `TrackRow` as a `qml/rows/TrackRow.qml` item object, with the (initially
/// empty) `artPath` slot the 36px art cell reads (`label_qt::track_row_value`).
fn track_row_value(row: &TrackRow) -> Value {
    let mut value = serde_json::to_value(row).unwrap_or_else(|_| Value::Object(Default::default()));
    if let Some(obj) = value.as_object_mut() {
        obj.insert("artPath".to_string(), Value::String(String::new()));
    }
    value
}

// --- mix track sourcing (mix.rs, verbatim minus the reco seeds) ------------

/// Even-spread sample of up to `n` ids (`mix.rs::pick_spread` / Tauri's
/// pickSpread): stride so the analysis seeds are not all clustered.
fn pick_spread(ids: &[u64], n: usize) -> Vec<u64> {
    if ids.len() <= n {
        return ids.to_vec();
    }
    (0..n).map(|i| ids[i * ids.len() / n]).collect()
}

/// The DailyQ / WeeklyQ listened-track seed: recent QOBUZ plays + Qobuz
/// favourites, deduped, capped at 120. Local / Plex / ephemeral recents carry
/// non-Qobuz ids and are excluded; a recents-only seed is frequently empty for
/// local-heavy users, so favourites guarantee a non-empty seed.
async fn mix_listened_seed_ids<A>(runtime: &Arc<AppRuntime<A>>) -> Vec<u64>
where
    A: FrontendAdapter + Send + Sync + 'static,
{
    let mut seeds: Vec<u64> = crate::recently_qt::load_tracks()
        .into_iter()
        .filter(|t| !matches!(t.source.as_str(), "local" | "plex" | "ephemeral"))
        .filter_map(|t| t.id.parse::<u64>().ok())
        .collect();
    let mut seen: HashSet<u64> = seeds.iter().copied().collect();
    for fav in favorite_tracks(runtime).await {
        if seen.insert(fav.id) {
            seeds.push(fav.id);
        }
    }
    seeds.truncate(120);
    seeds
}

/// Resolve up to 9 spread seeds into the `track_to_analysed` payload (the
/// PRIMARY DailyQ / WeeklyQ path, Tauri `buildSeeds`): `get_track` each,
/// extract `{track_id, artist_id, genre_id, label_id}` (artist = performer,
/// else composer; missing ids default to 0), drop any with `artist_id == 0`.
async fn build_tracks_to_analyse<A>(
    runtime: &Arc<AppRuntime<A>>,
    seeds: &[u64],
) -> Vec<TrackToAnalyse>
where
    A: FrontendAdapter + Send + Sync + 'static,
{
    let mut analysed = Vec::new();
    for id in pick_spread(seeds, 9) {
        let Ok(track) = runtime.core().get_track(id).await else {
            continue;
        };
        let artist_id = track
            .performer
            .as_ref()
            .map(|a| a.id)
            .or_else(|| track.composer.as_ref().map(|a| a.id))
            .unwrap_or(0);
        if artist_id == 0 {
            continue;
        }
        analysed.push(TrackToAnalyse {
            track_id: track.id,
            artist_id,
            genre_id: track
                .album
                .as_ref()
                .and_then(|a| a.genre.as_ref())
                .map(|g| g.id)
                .unwrap_or(0),
            label_id: track
                .album
                .as_ref()
                .and_then(|a| a.label.as_ref())
                .map(|l| l.id)
                .unwrap_or(0),
        });
    }
    analysed
}

async fn load_mix<A>(runtime: &Arc<AppRuntime<A>>, kind: &str) -> Vec<Track>
where
    A: FrontendAdapter + Send + Sync + 'static,
{
    match kind {
        "daily" | "weekly" => {
            let seeds = mix_listened_seed_ids(runtime).await;
            if seeds.is_empty() {
                log::warn!(
                    "[qbz-qt] mix '{kind}': no Qobuz seed tracks (recents + favorites empty) — empty mix"
                );
                return Vec::new();
            }
            let analysed = build_tracks_to_analyse(runtime, &seeds).await;
            let limit = (50usize.saturating_sub(analysed.len())).max(1) as u32;
            let tracks = match runtime
                .core()
                .get_dynamic_suggest_full(&seeds, &analysed, limit)
                .await
            {
                Ok(tracks) if !tracks.is_empty() => tracks,
                // FALLBACK (Tauri): retry with empty analysis + limit 50.
                Ok(_) => runtime
                    .core()
                    .get_dynamic_suggest(&seeds, 50)
                    .await
                    .unwrap_or_default(),
                Err(e) => {
                    log::warn!("[qbz-qt] mix '{kind}': dynamic/suggest failed: {e}");
                    Vec::new()
                }
            };
            log::info!(
                "[qbz-qt] mix '{kind}': {} seeds, {} analysed -> {} tracks",
                seeds.len(),
                analysed.len(),
                tracks.len()
            );
            tracks
        }
        "fav" => {
            let mut tracks = favorite_tracks(runtime).await;
            shuffle(&mut tracks);
            tracks
        }
        "top" => playlist_tracks(runtime).await,
        _ => Vec::new(),
    }
}

async fn favorite_tracks<A>(runtime: &Arc<AppRuntime<A>>) -> Vec<Track>
where
    A: FrontendAdapter + Send + Sync + 'static,
{
    match runtime.core().get_favorites("tracks", 200, 0).await {
        Ok(value) => qbz_models::lenient::parse_items_array(&value, "tracks", "mix favorite track"),
        Err(e) => {
            log::warn!("[qbz-qt] mix favorite tracks failed: {e}");
            Vec::new()
        }
    }
}

async fn playlist_tracks<A>(runtime: &Arc<AppRuntime<A>>) -> Vec<Track>
where
    A: FrontendAdapter + Send + Sync + 'static,
{
    let Ok(playlists) = runtime.core().get_user_playlists().await else {
        return Vec::new();
    };
    let mut out: Vec<Track> = Vec::new();
    for pl in playlists.into_iter().take(5) {
        if out.len() >= 100 {
            break;
        }
        if let Ok(full) = runtime.core().get_playlist(pl.id).await {
            if let Some(container) = full.tracks {
                out.extend(container.items);
            }
        }
    }
    out.truncate(100);
    out
}

/// Lightweight, deterministic-per-call shuffle (no rng dep) — `mix.rs`.
fn shuffle(tracks: &mut [Track]) {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
        | 1;
    let n = tracks.len();
    for i in (1..n).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let j = (seed % (i as u64 + 1)) as usize;
        tracks.swap(i, j);
    }
}

fn mmss(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Human total duration: "1 h 23 min" or "23 min" (`mix.rs::total_duration`).
fn total_duration(tracks: &[Track]) -> String {
    let secs: u64 = tracks.iter().map(|t| t.duration as u64).sum();
    let mins = secs / 60;
    if mins >= 60 {
        let h = (mins / 60).to_string();
        let m = (mins % 60).to_string();
        qbz_i18n::t_args("{} h {} min", &[&h, &m])
    } else {
        qbz_i18n::t_args("{} min", &[&mins.to_string()])
    }
}

/// `Track` -> the shared `album_qt::TrackRow` (rule 5: the row contract
/// `qml/rows/TrackRow.qml` already reads — the SAME struct the album, artist
/// and label pages serialize, which is the one that carries `album` for the
/// 220px Album column `showAlbum: true` draws).
///
/// `number` stays empty (the .slint mix rows carry none either — the index
/// comes from the delegate) and the work-grouping fields are album-detail
/// only, so they stay empty exactly as they do on the label page.
fn to_row(track: &Track) -> TrackRow {
    let mut title = track.title.clone();
    if let Some(v) = track.version.as_ref().filter(|v| !v.is_empty()) {
        title = format!("{title} ({v})");
    }
    let bit_depth = track.maximum_bit_depth;
    let sample_rate = track.maximum_sampling_rate;
    TrackRow {
        featured: Vec::new(),
        id: track.id.to_string(),
        title,
        artist: track
            .performer
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default(),
        artist_id: track
            .performer
            .as_ref()
            .map(|p| p.id.to_string())
            .unwrap_or_default(),
        album: track
            .album
            .as_ref()
            .map(|a| a.title.clone())
            .unwrap_or_default(),
        album_id: track
            .album
            .as_ref()
            .map(|a| a.id.clone())
            .unwrap_or_default(),
        duration: mmss(track.duration),
        quality_tier: crate::home_qt::quality_tier_from_depth(bit_depth).to_string(),
        quality_detail: crate::home_qt::quality_detail_from_parts(bit_depth, sample_rate),
        // See `TrackRow::bit_depth` — the raw numbers ride with the row.
        bit_depth,
        sample_rate,
        explicit: track.parental_warning,
        // Track row art: full variant (best()) — the thumbnail down-tier
        // was reverted after the 2026-08-15 owner smoke (contract 04 §3).
        artwork_url: track
            .album
            .as_ref()
            .and_then(|a| a.image.best().cloned())
            .unwrap_or_default(),
        is_favorite: crate::fav_cache_qt::is_favorite("track", &track.id.to_string()),
        cache_status: if crate::offline_qt::is_cached(&track.id.to_string()) {
            3
        } else {
            0
        },
        // NOT covered by `..default()`: that would leave every For You / mix row
        // affirmatively claiming to be available, while `to_queue_track` two
        // functions down reads the real value — the row and the queue built from
        // the SAME track would disagree, which is the one thing the row model
        // exists to prevent.
        not_streamable: !track.is_streamable(),
        ..TrackRow::default()
    }
}

/// `Track` -> `QueueTrack` (`playback_qt::catalog_queue_track`'s shape). The
/// mix is a FLAT list, so no container context is stamped — the now-playing
/// "playing from" falls back to each track's own album, 1:1 with the Slint
/// `play_tracks_ctx(.., None)` radio/mix path.
pub(crate) fn to_queue_track(track: &Track) -> QueueTrack {
    let (album_id, album_title, album_artwork) = match track.album.as_ref() {
        Some(album) => (
            album.id.clone(),
            album.title.clone(),
            // Queue row / np bar feed: full variant (best()) — the thumbnail
            // down-tier was reverted after the 2026-08-15 owner smoke (contract 04 §3).
            album.image.best().cloned().unwrap_or_default(),
        ),
        None => (String::new(), String::new(), String::new()),
    };
    // Register the full variant set for the immersive large-art feed (D2).
    if let Some(album) = track.album.as_ref() {
        if !album.id.is_empty() {
            crate::artwork_qt::note_np_variants(&album.id, &album.image);
        }
    }
    let album_key = if album_id.is_empty() {
        None
    } else {
        Some(album_id)
    };
    QueueTrack {
        id: track.id,
        title: track.title.clone(),
        version: track.version.clone(),
        artist: track
            .performer
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default(),
        album: album_title,
        album_version: None,
        duration_secs: track.duration as u64,
        artwork_url: if album_artwork.is_empty() {
            None
        } else {
            Some(album_artwork)
        },
        hires: track.hires,
        bit_depth: track.maximum_bit_depth,
        sample_rate: track.maximum_sampling_rate,
        is_local: false,
        album_id: album_key.clone(),
        artist_id: track.performer.as_ref().map(|p| p.id),
        // §3.1: absence is resolved in ONE place. `is_streamable()` reads a
        // missing key as AVAILABLE — only an explicit `false` marks a pull.
        streamable: track.is_streamable(),
        source: Some("qobuz".to_string()),
        parental_warning: track.parental_warning,
        source_item_id_hint: album_key,
        context_kind: None,
        context_id: None,
        isrc: track.isrc.clone(),
        recording_mbid: None,
    }
}

// --- mix playback ---------------------------------------------------------

fn mix_queue() -> Vec<QueueTrack> {
    MIX.lock()
        .ok()
        .and_then(|s| s.as_ref().map(|state| state.queue.clone()))
        .unwrap_or_default()
}

/// Header Play — the whole mix becomes the queue, from the top.
pub(crate) fn mix_play_all() {
    play_queue_from(0, false);
}

/// Header Shuffle — a fresh random order that does NOT mutate the displayed
/// list (`mix.rs::shuffled_tracks`).
pub(crate) fn mix_shuffle() {
    let mut queue = mix_queue();
    if queue.is_empty() {
        return;
    }
    shuffle_queue(&mut queue);
    let runtime = crate::app();
    crate::spawn(async move {
        if let Err(e) = crate::playback_qt::play_track_list(&runtime, queue, 0, false).await {
            log::warn!("[qbz-qt] mix shuffle failed: {e}");
        }
    });
}

/// A row click / row Play — the mix becomes the queue anchored at that track
/// (`mix.rs::index_of` + play-from-here).
pub(crate) fn mix_play_track(track_id: String) {
    let index = mix_queue()
        .iter()
        .position(|t| t.id.to_string() == track_id)
        .unwrap_or(0);
    play_queue_from(index, false);
}

/// A row's ⋯ menu: "next" | "later" | "queue".
pub(crate) fn mix_enqueue_track(track_id: String, mode: String) {
    let Some(track) = mix_queue()
        .into_iter()
        .find(|t| t.id.to_string() == track_id)
    else {
        return;
    };
    let runtime = crate::app();
    crate::spawn(async move {
        if let Err(e) =
            crate::playback_qt::enqueue_track_list_mode(&runtime, vec![track], &mode).await
        {
            log::warn!("[qbz-qt] mix enqueue failed: {e}");
        }
    });
}

fn play_queue_from(index: usize, shuffle: bool) {
    let queue = mix_queue();
    if queue.is_empty() {
        return;
    }
    let runtime = crate::app();
    crate::spawn(async move {
        if let Err(e) = crate::playback_qt::play_track_list(&runtime, queue, index, shuffle).await {
            log::warn!("[qbz-qt] mix play failed: {e}");
        }
    });
}

/// Shuffle over the already-built queue rows (the mix's own xorshift, applied
/// to `QueueTrack` instead of `Track`).
fn shuffle_queue(tracks: &mut [QueueTrack]) {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
        | 1;
    let n = tracks.len();
    for i in (1..n).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let j = (seed % (i as u64 + 1)) as usize;
        tracks.swap(i, j);
    }
}

/// Start a flat radio result as the queue (Slint `play_radio_response` ->
/// `play_tracks(.., 0)`), reusing the port's shared list-play path.
async fn play_flat(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    tracks: Vec<Track>,
    start: usize,
    shuffle: bool,
) {
    let queue: Vec<QueueTrack> = tracks.iter().map(to_queue_track).collect();
    if queue.is_empty() {
        return;
    }
    if let Err(e) = crate::playback_qt::play_track_list(runtime, queue, start, shuffle).await {
        log::warn!("[qbz-qt] radio play failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_spread_strides_and_never_overruns() {
        let ids: Vec<u64> = (0..100).collect();
        let picked = pick_spread(&ids, 9);
        assert_eq!(picked.len(), 9);
        assert_eq!(picked[0], 0);
        // Strictly increasing, in range.
        for w in picked.windows(2) {
            assert!(w[0] < w[1]);
        }
        assert!(picked.iter().all(|id| *id < 100));
        // Fewer ids than asked for -> the whole list, unchanged.
        assert_eq!(pick_spread(&ids[..4], 9), ids[..4].to_vec());
    }

    #[test]
    fn mix_meta_covers_the_four_kinds_and_degrades() {
        for kind in ["daily", "weekly", "fav", "top"] {
            let (title, subtitle) = mix_meta(kind);
            assert!(!title.is_empty(), "{kind} has no title");
            assert!(!subtitle.is_empty(), "{kind} has no subtitle");
        }
        assert_eq!(mix_meta("nope").0, "Mix");
    }

    #[test]
    fn build_radio_dedupes_caps_and_drops_non_qobuz_sources() {
        let recents = vec![
            crate::recently_qt::RecentAlbum {
                id: "a1".into(),
                title: "One".into(),
                artist: "X".into(),
                source: "qobuz".into(),
                ..Default::default()
            },
            // Same album again — deduped.
            crate::recently_qt::RecentAlbum {
                id: "a1".into(),
                title: "One".into(),
                artist: "X".into(),
                source: String::new(),
                ..Default::default()
            },
            // Plex ids cannot resolve against /radio/album.
            crate::recently_qt::RecentAlbum {
                id: "plex-7".into(),
                title: "Nope".into(),
                artist: "Y".into(),
                source: "plex".into(),
                ..Default::default()
            },
            // Legacy entry with no source is treated as Qobuz.
            crate::recently_qt::RecentAlbum {
                id: "a2".into(),
                title: "Two".into(),
                artist: "Z".into(),
                source: String::new(),
                ..Default::default()
            },
        ];
        let rows = build_radio(&recents, &[]);
        assert_eq!(
            rows.iter().map(|r| r.album_id.as_str()).collect::<Vec<_>>(),
            vec!["a1", "a2"]
        );
    }
}
