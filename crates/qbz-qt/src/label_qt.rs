//! Label landing page + "See all releases" sub-view — the Slint-free port
//! of `crates/qbz/src/label.rs` (`load_label_page`, `build_label_jump_tabs`,
//! `extract_label_image`, `derive_releases`, `sort_album_items`).
//!
//! ONE module for BOTH routes on purpose: the Slint keeps them in one
//! `label.rs` too, and they share the header (name / logo / id), the follow
//! state and the same page fetch. Splitting them would duplicate all three.
//!
//! Two documents on the bridge:
//!   `labelJson`          -> `qml/views/LabelView.qml`          (route "label")
//!   `labelReleasesJson`  -> `qml/views/LabelReleasesView.qml`  ("labelreleases")
//!
//! POC-NOTEs (deliberate deltas vs LabelPageView.slint):
//! - **In library tab**: the Slint seeds it from its session
//!   `library_by_label` index, which this port does not build (the Qt
//!   library feed carries no label id). `libraryCount` is therefore always
//!   0, the catalog/library `SegmentedTabBar` never mounts, and the library
//!   branch is absent — rather than a toggle that switches to an empty tab.
//! - **Popular Tracks multi-select**: the shared row and destination bridges
//!   are live, but this page does not yet own a selection model or mount the
//!   bulk bar. See the view's header note.
//! - **Blacklist**: the premise of this note ("`crate::artist_blacklist` does
//!   not exist in this port") is DEAD — the module is bound at `auth_qt.rs`
//!   and the QUEUE is filtered since PARITY-DEBT #1. What is still missing is
//!   the LISTING drop: the Slint filters the releases / critics rails through
//!   `album_blacklisted` (label.rs:92, :133, :537, :567) and stamps the row
//!   flag (:900); `derive_releases` here does neither, so a blocked artist's
//!   album still RENDERS on a label page (playing it is already blocked).
//! - **Share** is DONE and Rust-side (`QbzHome::label_share` →
//!   `share_qt::share_label`), 1:1 with `main.rs:13164-13178`: the
//!   `play.qobuz.com/label/{id}` link plus the "Link copied" success toast.
//!   It used to be a QML clipboard write that formatted
//!   `open.qobuz.com/label/{id}` — a host with NO `/label/` route, so the
//!   copied link was dead on arrival (owner-reported 2026-08-02). The host is
//!   per-entity, not per-app: track/album on `open.`, artist/playlist/label on
//!   `play.` — which is exactly why the URL builders live in one place now.
//! - **Scroll restore** is live through the shared `ScrollMemory.qml` seam.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use cxx_qt_lib::QString;
use qbz_models::{Album, QueueTrack};
use serde::Serialize;
use serde_json::Value;

use crate::album_qt::TrackRow;
use crate::home_qt::{attach_card_art, HomeCard};

/// `/label/getAlbums` page size (label.rs:28 — Tauri pulls 500 so a typical
/// label loads in one shot).
pub const RELEASES_PAGE_SIZE: u32 = 500;

// ===========================================================================
//  Documents
// ===========================================================================

#[derive(Clone, Default, Serialize)]
pub struct JumpTab {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Default, Serialize)]
struct LabelDoc {
    id: String,
    name: String,
    #[serde(rename = "imageUrl")]
    image_url: String,
    /// `file://…` for the header logo once the cache resolves it.
    #[serde(rename = "artPath")]
    art_path: String,
    description: String,
    #[serde(rename = "descriptionShort")]
    description_short: String,
    #[serde(rename = "descriptionTruncated")]
    description_truncated: bool,
    #[serde(rename = "isFollowing")]
    is_following: bool,
    #[serde(rename = "jumpTabs")]
    jump_tabs: Vec<JumpTab>,
    /// Popular Tracks as `qml/rows/TrackRow.qml` item objects. Serialized
    /// `TrackRow`s PLUS an `artPath` key: that struct is shared with the
    /// album / artist pages and carries only `artUrl`, while TrackRow.qml's
    /// 36px art cell reads `item.artPath`. Patching the value here keeps ONE
    /// artwork mechanism on this page (Rust-resolved `artPath`, the HomeView
    /// pattern) instead of grafting ArtistView's per-view `coverMap` on top.
    #[serde(rename = "topTracks")]
    top_tracks: Vec<Value>,
    releases: Vec<HomeCard>,
    critics: Vec<HomeCard>,
    playlists: Vec<HomeCard>,
    artists: Vec<HomeCard>,
    #[serde(rename = "moreLabels")]
    more_labels: Vec<HomeCard>,
    /// Always 0 in this port — see the module POC-NOTE.
    #[serde(rename = "libraryCount")]
    library_count: u32,
}

#[derive(Clone, Default, Serialize)]
struct ReleasesDoc {
    id: String,
    name: String,
    #[serde(rename = "imageUrl")]
    image_url: String,
    #[serde(rename = "artPath")]
    art_path: String,
    total: u32,
    shown: u32,
    #[serde(rename = "hiresCount")]
    hires_count: u32,
    #[serde(rename = "hasMore")]
    has_more: bool,
    #[serde(rename = "loadMoreLoading")]
    load_more_loading: bool,
    #[serde(rename = "groupBy")]
    group_by: bool,
    #[serde(rename = "filterHires")]
    filter_hires: bool,
    #[serde(rename = "sortBy")]
    sort_by: String,
    query: String,
    visible: Vec<HomeCard>,
    grouped: Vec<GroupedSection>,
}

#[derive(Clone, Default, Serialize)]
struct GroupedSection {
    title: String,
    albums: Vec<HomeCard>,
}

// ===========================================================================
//  State
// ===========================================================================

struct LabelState {
    id: String,
    doc: LabelDoc,
    loading: bool,
    generation: u64,
    /// Popular Tracks as a playable queue — the source behind Play / Shuffle
    /// and the two ⋯ queue menus (label.rs `PLAY_TOP_TRACKS`).
    play_queue: Vec<QueueTrack>,
    // --- releases sub-view -------------------------------------------------
    /// The FULL loaded catalog; `visible` / `grouped` derive from it.
    albums: Vec<HomeCard>,
    releases_offset: u32,
    releases_has_more: bool,
    releases_total: u32,
    releases_loading: bool,
    load_more_loading: bool,
    sort_by: String,
    filter_hires: bool,
    group_by: bool,
    query: String,
    releases_generation: u64,
}

impl LabelState {
    const fn new() -> Self {
        Self {
            id: String::new(),
            doc: LabelDoc {
                id: String::new(),
                name: String::new(),
                image_url: String::new(),
                art_path: String::new(),
                description: String::new(),
                description_short: String::new(),
                description_truncated: false,
                is_following: false,
                jump_tabs: Vec::new(),
                top_tracks: Vec::new(),
                releases: Vec::new(),
                critics: Vec::new(),
                playlists: Vec::new(),
                artists: Vec::new(),
                more_labels: Vec::new(),
                library_count: 0,
            },
            loading: false,
            generation: 0,
            play_queue: Vec::new(),
            albums: Vec::new(),
            releases_offset: 0,
            releases_has_more: false,
            releases_total: 0,
            releases_loading: false,
            load_more_loading: false,
            sort_by: String::new(),
            filter_hires: false,
            group_by: false,
            query: String::new(),
            releases_generation: 0,
        }
    }
}

static LABEL: Mutex<LabelState> = Mutex::new(LabelState::new());

fn publish_label() {
    let (json, loading) = {
        let Ok(s) = LABEL.lock() else {
            return;
        };
        (
            serde_json::to_string(&s.doc).unwrap_or_else(|_| "{}".to_string()),
            s.loading,
        )
    };
    crate::home_bridge::ui(move |mut b| {
        b.as_mut().set_label_json(QString::from(json.as_str()));
        b.as_mut().set_label_loading(loading);
    });
}

// ===========================================================================
//  Landing page
// ===========================================================================

/// Open the label landing page. Same order as `main.rs::open_album`:
/// offline gate -> `nav_qt::record` (which already publishes `currentView`)
/// -> clear the document + raise `loading` in ONE hop -> fetch.
pub fn open_label(label_id: String) {
    if crate::offline_fwd::engine().status().is_offline() {
        return;
    }
    let Ok(id) = label_id.trim().parse::<u64>() else {
        log::warn!("[qbz-qt] open_label: not a Qobuz label id: {label_id}");
        return;
    };
    crate::nav_qt::record("label");
    let generation = {
        let Ok(mut s) = LABEL.lock() else {
            return;
        };
        s.generation = s.generation.wrapping_add(1);
        s.id = id.to_string();
        s.doc = LabelDoc {
            id: id.to_string(),
            ..LabelDoc::default()
        };
        s.loading = true;
        s.play_queue.clear();
        s.generation
    };
    publish_label();
    fetch_label(generation, id);
}

fn fetch_label(generation: u64, id: u64) {
    let runtime = crate::app();
    crate::spawn(async move {
        let page = match runtime.core().get_label_page(id).await {
            Ok(page) => page,
            Err(e) => {
                log::warn!("[qbz-qt] label page load failed ({id}): {e}");
                if let Ok(mut s) = LABEL.lock() {
                    if s.generation == generation {
                        s.loading = false;
                    }
                }
                publish_label();
                return;
            }
        };

        let follow_ids = favorite_label_ids(&runtime).await;

        let name = page.name.clone();
        let image_url = extract_label_image(page.image.as_ref());
        let description = page
            .description
            .as_deref()
            .map(qbz_text_utils::strip_html::strip_html)
            .unwrap_or_default();
        let description_short = crate::album_qt::truncate_words(&description, 360);
        let description_truncated = description_short != description;

        let raw_top = page.top_tracks.clone().unwrap_or_default();
        let track_rows: Vec<TrackRow> = raw_top
            .iter()
            .enumerate()
            .map(|(i, v)| parse_top_track(i, v))
            .collect();
        let queue: Vec<QueueTrack> = track_rows.iter().map(track_row_to_queue).collect();
        let top_tracks: Vec<Value> = track_rows.iter().map(track_row_value).collect();

        // Critics' Picks — the `releases` container whose id mentions
        // award/critic/press (label.rs:516-542).
        let critics: Vec<HomeCard> = page
            .releases
            .as_ref()
            .and_then(|containers| {
                containers
                    .iter()
                    .find(|c| {
                        c.id.as_deref()
                            .map(|id| {
                                let id = id.to_lowercase();
                                id.contains("award")
                                    || id.contains("critic")
                                    || id.contains("press")
                            })
                            .unwrap_or(false)
                    })
                    .and_then(|c| c.data.as_ref())
                    .and_then(|d| d.items.as_ref())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| serde_json::from_value::<Album>(v.clone()).ok())
                            .map(crate::home_qt::map_flat_album)
                            .collect()
                    })
            })
            .unwrap_or_default();

        let playlists: Vec<HomeCard> = page
            .playlists
            .as_ref()
            .and_then(|p| p.items.as_ref())
            .map(|items| items.iter().map(parse_playlist).collect())
            .unwrap_or_default();

        let artists: Vec<HomeCard> = page
            .top_artists
            .as_ref()
            .and_then(|a| a.items.as_ref())
            .map(|items| items.iter().map(parse_artist).collect())
            .unwrap_or_default();

        // Releases carousel — the first 20 of /label/getAlbums.
        let releases: Vec<HomeCard> = match runtime
            .core()
            .get_label_albums(id, 20, 0, None, None, None, None, None)
            .await
        {
            Ok(p) => p
                .items
                .into_iter()
                .map(crate::home_qt::map_flat_album)
                .collect(),
            Err(e) => {
                log::warn!("[qbz-qt] label releases carousel failed: {e}");
                Vec::new()
            }
        };

        // More labels — /label/explore minus the current one.
        let more_labels: Vec<HomeCard> = match runtime.core().get_label_explore(20, 0).await {
            Ok(resp) => parse_more_labels(&resp, id),
            Err(e) => {
                log::warn!("[qbz-qt] label explore failed: {e}");
                Vec::new()
            }
        };

        let mut doc = LabelDoc {
            id: id.to_string(),
            name,
            image_url,
            art_path: String::new(),
            description,
            description_short,
            description_truncated,
            is_following: follow_ids.contains(&id),
            jump_tabs: Vec::new(),
            top_tracks,
            releases,
            critics,
            playlists,
            artists,
            more_labels,
            library_count: 0,
        };
        doc.jump_tabs = build_jump_tabs(&doc);

        // Covers: one attach pass over every section + the header logo.
        let mut missing = attach_doc_art(&mut doc);
        if !doc.image_url.is_empty() {
            let hit = crate::artwork_qt::cached_path(&doc.image_url);
            if hit.is_empty() {
                missing.push(doc.image_url.clone());
            } else {
                doc.art_path = hit;
            }
        }

        {
            let Ok(mut s) = LABEL.lock() else {
                return;
            };
            if s.generation != generation {
                return;
            }
            s.doc = doc;
            s.play_queue = queue;
            s.loading = false;
        }
        publish_label();

        if !missing.is_empty() {
            crate::artwork_qt::download_missing(missing).await;
            refresh_label_art(generation);
        }

        // Label summaries can omit artist portraits entirely. Enrich only
        // those entries after publishing the page, one request at a time.
        let missing_artists: std::collections::BTreeSet<u64> = {
            let Ok(s) = LABEL.lock() else { return };
            if s.generation != generation {
                return;
            }
            s.doc
                .artists
                .iter()
                .filter(|card| card.art_url.is_empty())
                .filter_map(|card| card.id.parse().ok())
                .collect()
        };
        for artist_id in missing_artists {
            if !LABEL
                .lock()
                .map(|s| s.generation == generation)
                .unwrap_or(false)
            {
                return;
            }
            let artist = match runtime.core().get_artist(artist_id).await {
                Ok(artist) => artist,
                Err(error) => {
                    log::debug!(
                        "[qbz-qt] label artist portrait lookup failed ({artist_id}): {error}"
                    );
                    continue;
                }
            };
            let Some(url) = artist
                .image
                .as_ref()
                .and_then(|image| image.for_px(400))
                .filter(|url| !url.is_empty())
                .cloned()
            else {
                continue;
            };
            {
                let Ok(mut s) = LABEL.lock() else { return };
                if s.generation != generation {
                    return;
                }
                for card in &mut s.doc.artists {
                    if card.id == artist_id.to_string() && card.art_url.is_empty() {
                        card.art_url = url.clone();
                    }
                }
            }
            crate::artwork_qt::download_missing(vec![url]).await;
            refresh_label_art(generation);
        }
    });
}

/// Re-run the cache lookup over every section after the downloads settled.
fn refresh_label_art(generation: u64) {
    let mut doc = {
        let Ok(mut s) = LABEL.lock() else {
            return;
        };
        if s.generation != generation {
            return;
        }
        std::mem::take(&mut s.doc)
    };
    let _ = attach_doc_art(&mut doc);
    if !doc.image_url.is_empty() {
        doc.art_path = crate::artwork_qt::cached_path(&doc.image_url);
    }
    {
        let Ok(mut s) = LABEL.lock() else {
            return;
        };
        if s.generation != generation {
            return;
        }
        s.doc = doc;
    }
    publish_label();
}

/// Attach cached covers to EVERY card list on the document and return the
/// urls still missing. THE RULE: a section added below has to be listed
/// here, or its tiles stay blank forever with nothing reporting it (the
/// ArtistView.qml `dispatchCovers` rule, Rust-side).
fn attach_doc_art(doc: &mut LabelDoc) -> Vec<String> {
    let mut missing = Vec::new();
    missing.extend(attach_card_art(&mut doc.releases));
    missing.extend(attach_card_art(&mut doc.critics));
    missing.extend(attach_card_art(&mut doc.playlists));
    missing.extend(attach_card_art(&mut doc.artists));
    missing.extend(attach_card_art(&mut doc.more_labels));
    // Popular-track thumbs ride the same cache but are not HomeCards — the
    // `artPath` key is patched straight onto the row object.
    for row in doc.top_tracks.iter_mut() {
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
/// empty) `artPath` slot the 36px art cell reads.
fn track_row_value(row: &TrackRow) -> Value {
    let mut value = serde_json::to_value(row).unwrap_or_else(|_| Value::Object(Default::default()));
    if let Some(obj) = value.as_object_mut() {
        obj.insert("artPath".to_string(), Value::String(String::new()));
        // `isFavorite` used to be hard-written false here, because the port
        // had no favourites cache. It has one now (`fav_cache_qt`) and
        // `TrackRow::is_favorite` carries the seeded value through the
        // serialization above — overwriting it would put the empty heart back
        // on tracks the user HAS favourited.
    }
    value
}

// ===========================================================================
//  Landing-page actions
// ===========================================================================

/// Header Follow / Following.
///
/// The current state comes off the PUBLISHED document, never off the library
/// feed: `library_qt::toggle_favorite` derives it from `with_library(...)`,
/// which returns `None` when the Library view was never opened this session
/// — the call would then silently no-op, i.e. exactly the "renders and does
/// nothing" defect. This calls `add_favorite` / `remove_favorite` directly
/// and reverts the optimistic flip on failure.
pub fn toggle_follow() {
    let (id, next) = {
        let Ok(mut s) = LABEL.lock() else {
            return;
        };
        if s.doc.id.is_empty() {
            return;
        }
        let next = !s.doc.is_following;
        s.doc.is_following = next;
        (s.doc.id.clone(), next)
    };
    publish_label();
    let runtime = crate::app();
    crate::spawn(async move {
        // SINGULAR — `/favorite/create|delete` take `label_ids`, and the
        // client derives that key as `format!("{fav_type}_ids")`. The plural
        // spelling belongs to `/favorite/getUserFavorites`'s `type` param
        // (see `favorite_label_ids` below); sending it here produced
        // `labels_ids=…`, an unknown param — create no-opped, delete 404'd.
        // Reference: qbz/src/main.rs:13054 `add_favorite("label", …)`.
        let result = if next {
            runtime.core().add_favorite("label", &id).await
        } else {
            runtime.core().remove_favorite("label", &id).await
        };
        if result.is_ok() {
            // Keep the cache in step so re-opening the page (or the next
            // launch, or a failed `getUserFavorites`) shows what just
            // happened instead of the pre-toggle state.
            crate::fav_cache_qt::set("label", &id, next);
            // …and through the shared settle point, so the Library feed row
            // and the document caches learn about it too. A label followed
            // from here otherwise stayed un-followed in the Library grid
            // until the next full reload.
            crate::emit_library_favorite("label", &id, next);
        }
        if let Err(e) = result {
            log::warn!("[qbz-qt] label follow toggle failed ({id}): {e}");
            {
                let Ok(mut s) = LABEL.lock() else {
                    return;
                };
                if s.doc.id == id {
                    s.doc.is_following = !next;
                }
            }
            publish_label();
        }
    });
}

/// Popular Tracks Play (`shuffle == false`) / header Shuffle (`true`).
///
/// PARITY-DEBT #17. `shuffle` is a ONE-SHOT reorder of this queue, played from
/// index 0 — `play_label_top_shuffled` (playback.rs:3120-3150) xorshift-mixes
/// the Vec and hands it to `play_tracks_ctx`; it never touches the player's
/// shuffle MODE, which is why the bar's toggle must stay exactly where the
/// user left it after pressing Shuffle here. The reorder itself lives in
/// `playback_qt::play_track_list_in` (one implementation for every caller of
/// the flag). The label context is EXPLICIT because a label's popular tracks
/// share neither album nor artist and nothing could derive it.
pub fn play_top(shuffle: bool) {
    let (queue, label_id) = {
        let Ok(s) = LABEL.lock() else {
            return;
        };
        (s.play_queue.clone(), s.id.clone())
    };
    if queue.is_empty() {
        log::info!("[qbz-qt] label play-top: no playable tracks");
        return;
    }
    let runtime = crate::app();
    crate::spawn(async move {
        let ctx = crate::playback_qt::PlayContext::label(&label_id);
        if let Err(e) =
            crate::playback_qt::play_track_list_in(&runtime, queue, 0, shuffle, ctx).await
        {
            log::error!("[qbz-qt] label play-top failed: {e}");
        }
    });
}

/// One Popular-Tracks row click (the whole list becomes the queue, starting
/// at the clicked track — the ArtistView row-play contract).
pub fn play_track(track_id: String) {
    let Ok(wanted) = track_id.parse::<u64>() else {
        return;
    };
    let (queue, start, label_id) = {
        let Ok(s) = LABEL.lock() else {
            return;
        };
        let start = s
            .play_queue
            .iter()
            .position(|t| t.id == wanted)
            .unwrap_or(0);
        (s.play_queue.clone(), start, s.id.clone())
    };
    if queue.is_empty() {
        return;
    }
    let runtime = crate::app();
    crate::spawn(async move {
        // Same explicit label origin as the header play (the ContentView::Label
        // arm of `play_track_in_context`, playback.rs:3527-3543):
        // a row click on a label page is still "playing from" the label.
        let ctx = crate::playback_qt::PlayContext::label(&label_id);
        if let Err(e) =
            crate::playback_qt::play_track_list_in(&runtime, queue, start, false, ctx).await
        {
            log::error!("[qbz-qt] label row play failed: {e}");
        }
    });
}

/// The ⋯ menus' three all-tracks queue entries — "next" inserts at the
/// cursor (fed reversed so the list keeps its order), "later" appends to the
/// manual block's tail, "queue" appends. Each label does what it says.
pub fn enqueue_top(mode: String) {
    let queue = {
        let Ok(s) = LABEL.lock() else {
            return;
        };
        s.play_queue.clone()
    };
    if queue.is_empty() {
        log::info!("[qbz-qt] label enqueue-top: no playable tracks");
        return;
    }
    let runtime = crate::app();
    crate::spawn(async move {
        if let Err(e) = crate::playback_qt::enqueue_track_list_mode(&runtime, queue, &mode).await {
            log::error!("[qbz-qt] label enqueue-top ({mode}) failed: {e}");
        }
    });
}

/// Popular-Tracks row "Play next" / "Play later" / "Add to queue".
pub fn enqueue_track(track_id: String, mode: String) {
    let Ok(id) = track_id.parse::<u64>() else {
        return;
    };
    let runtime = crate::app();
    crate::spawn(async move {
        if let Err(e) = crate::playback_qt::enqueue_single_track(&runtime, id, &mode).await {
            log::error!("[qbz-qt] label row enqueue ({mode}) failed: {e}");
        }
    });
}

// ===========================================================================
//  Releases sub-view
// ===========================================================================

fn publish_releases() {
    let (json, loading) = {
        let Ok(s) = LABEL.lock() else {
            return;
        };
        let (visible, grouped, shown, hires_count) = derive_releases(&s);
        let doc = ReleasesDoc {
            id: s.id.clone(),
            name: s.doc.name.clone(),
            image_url: s.doc.image_url.clone(),
            art_path: s.doc.art_path.clone(),
            total: s.releases_total,
            shown,
            hires_count,
            has_more: s.releases_has_more,
            load_more_loading: s.load_more_loading,
            group_by: s.group_by,
            filter_hires: s.filter_hires,
            sort_by: s.sort_by.clone(),
            query: s.query.clone(),
            visible,
            grouped,
        };
        (
            serde_json::to_string(&doc).unwrap_or_else(|_| "{}".to_string()),
            s.releases_loading,
        )
    };
    crate::home_bridge::ui(move |mut b| {
        b.as_mut()
            .set_label_releases_json(QString::from(json.as_str()));
        b.as_mut().set_label_releases_loading(loading);
    });
}

/// Releases carousel "View all". Resets the toolbar to its defaults for the
/// fresh label (label.rs `reset_label`:278-296).
pub fn open_releases() {
    if crate::offline_fwd::engine().status().is_offline() {
        return;
    }
    let (generation, id) = {
        let Ok(mut s) = LABEL.lock() else {
            return;
        };
        let Ok(id) = s.id.parse::<u64>() else {
            return;
        };
        s.releases_generation = s.releases_generation.wrapping_add(1);
        s.albums.clear();
        s.releases_offset = 0;
        s.releases_has_more = false;
        s.releases_total = 0;
        s.releases_loading = true;
        s.load_more_loading = false;
        s.sort_by = "newest".to_string();
        s.filter_hires = false;
        s.group_by = false;
        s.query.clear();
        (s.releases_generation, id)
    };
    crate::nav_qt::record("labelreleases");
    publish_releases();
    fetch_releases(generation, id);
}

pub fn releases_load_more() {
    let (generation, id) = {
        let Ok(mut s) = LABEL.lock() else {
            return;
        };
        if s.releases_loading || s.load_more_loading || !s.releases_has_more {
            return;
        }
        let Ok(id) = s.id.parse::<u64>() else {
            return;
        };
        s.load_more_loading = true;
        (s.releases_generation, id)
    };
    publish_releases();
    fetch_releases(generation, id);
}

pub fn releases_set_sort(sort: String) {
    {
        let Ok(mut s) = LABEL.lock() else {
            return;
        };
        s.sort_by = sort;
    }
    publish_releases();
}

pub fn releases_set_hires(on: bool) {
    {
        let Ok(mut s) = LABEL.lock() else {
            return;
        };
        s.filter_hires = on;
    }
    publish_releases();
}

pub fn releases_set_group(on: bool) {
    {
        let Ok(mut s) = LABEL.lock() else {
            return;
        };
        s.group_by = on;
    }
    publish_releases();
}

pub fn releases_search(query: String) {
    {
        let Ok(mut s) = LABEL.lock() else {
            return;
        };
        s.query = query;
    }
    publish_releases();
}

fn fetch_releases(generation: u64, id: u64) {
    let runtime = crate::app();
    let offset = LABEL.lock().map(|s| s.releases_offset).unwrap_or(0);
    crate::spawn(async move {
        match runtime
            .core()
            .get_label_albums(id, RELEASES_PAGE_SIZE, offset, None, None, None, None, None)
            .await
        {
            Ok(page) => {
                let item_count = page.items.len();
                let loaded = offset as usize + item_count;
                let total = page.total.map(|t| t as usize).unwrap_or(loaded);
                // label.rs:126-128 — trust the flag, fall back to "the page
                // came back full" (a short page means the catalog is done).
                let has_more = page
                    .has_more
                    .unwrap_or(item_count >= RELEASES_PAGE_SIZE as usize || total > loaded);
                let mut cards: Vec<HomeCard> = page
                    .items
                    .into_iter()
                    .map(crate::home_qt::map_flat_album)
                    .collect();
                let missing = attach_card_art(&mut cards);
                {
                    let Ok(mut s) = LABEL.lock() else {
                        return;
                    };
                    if s.releases_generation != generation {
                        return;
                    }
                    s.albums.extend(cards);
                    s.releases_offset = offset.saturating_add(item_count as u32);
                    s.releases_total = total as u32;
                    s.releases_has_more = has_more && item_count > 0;
                    s.releases_loading = false;
                    s.load_more_loading = false;
                }
                publish_releases();
                if !missing.is_empty() {
                    crate::artwork_qt::download_missing(missing).await;
                    let mut cards = {
                        let Ok(mut s) = LABEL.lock() else {
                            return;
                        };
                        if s.releases_generation != generation {
                            return;
                        }
                        std::mem::take(&mut s.albums)
                    };
                    let _ = attach_card_art(&mut cards);
                    {
                        let Ok(mut s) = LABEL.lock() else {
                            return;
                        };
                        if s.releases_generation != generation {
                            return;
                        }
                        s.albums = cards;
                    }
                    publish_releases();
                }
            }
            Err(e) => {
                log::warn!("[qbz-qt] label releases page failed ({id}): {e}");
                {
                    let Ok(mut s) = LABEL.lock() else {
                        return;
                    };
                    if s.releases_generation != generation {
                        return;
                    }
                    s.releases_loading = false;
                    s.load_more_loading = false;
                    s.releases_has_more = false;
                }
                publish_releases();
            }
        }
    });
}

/// `label.rs::derive_releases` — returns (visible, grouped, shown,
/// hires_count).
///
/// `hires_count` is computed over the FULL loaded catalog, BEFORE filtering
/// (label.rs:190-193): the toolbar line reports the catalog's Hi-Res total,
/// not the filtered one.
fn derive_releases(s: &LabelState) -> (Vec<HomeCard>, Vec<GroupedSection>, u32, u32) {
    let hires_count = s
        .albums
        .iter()
        .filter(|a| a.quality_tier == "hires")
        .count() as u32;

    let needle = s.query.trim().to_lowercase();
    let sort = if s.sort_by.is_empty() {
        "newest"
    } else {
        s.sort_by.as_str()
    };

    // Fast path: default order, no filter/search, flat.
    if !s.filter_hires && needle.is_empty() && sort == "newest" && !s.group_by {
        let shown = s.albums.len() as u32;
        return (s.albums.clone(), Vec::new(), shown, hires_count);
    }

    let mut filtered: Vec<HomeCard> = s
        .albums
        .iter()
        .filter(|a| {
            (!s.filter_hires || a.quality_tier == "hires")
                && (needle.is_empty()
                    || a.title.to_lowercase().contains(&needle)
                    || a.artist.to_lowercase().contains(&needle))
        })
        .cloned()
        .collect();
    sort_cards(&mut filtered, sort);
    let shown = filtered.len() as u32;

    if !s.group_by {
        return (filtered, Vec::new(), shown, hires_count);
    }

    // One section per artist, in first-appearance (already sorted) order.
    let mut sections: Vec<GroupedSection> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for item in filtered {
        let key = item.artist.clone();
        let idx = *index.entry(key.clone()).or_insert_with(|| {
            sections.push(GroupedSection {
                title: if key.is_empty() {
                    qbz_i18n::t("Unknown")
                } else {
                    key.clone()
                },
                albums: Vec::new(),
            });
            sections.len() - 1
        });
        sections[idx].albums.push(item);
    }
    (Vec::new(), sections, shown, hires_count)
}

/// Sort on original release dates, never the localized card label.
fn sort_cards(items: &mut [HomeCard], sort: &str) {
    match sort {
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

// ===========================================================================
//  Live language switch
// ===========================================================================

/// Re-derive the Rust-built strings on both label documents after a language
/// change: the JUMP TO tab labels and the "Unknown" group bucket.
///
/// Called by `main.rs::apply_language`; `trRev` cannot retranslate strings
/// that already arrived inside a JSON document.
pub fn republish_for_language() {
    {
        if let Ok(mut s) = LABEL.lock() {
            let tabs = build_jump_tabs(&s.doc);
            s.doc.jump_tabs = tabs;
        }
    }
    publish_label();
    publish_releases();
}

// ===========================================================================
//  JUMP TO tabs
// ===========================================================================

/// `label.rs::build_label_jump_tabs` — only sections with content.
///
/// The labels are their OWN msgids, distinct from the carousel titles: the
/// tab reads "More Labels" (capital L) while the carousel above it reads
/// "More labels". Collapsing them would break the catalogs.
fn build_jump_tabs(doc: &LabelDoc) -> Vec<JumpTab> {
    let mut tabs = vec![JumpTab {
        id: "about".to_string(),
        label: qbz_i18n::t("About"),
    }];
    let push = |tabs: &mut Vec<JumpTab>, id: &str, msgid: &str, present: bool| {
        if present {
            tabs.push(JumpTab {
                id: id.to_string(),
                label: qbz_i18n::t(msgid),
            });
        }
    };
    push(
        &mut tabs,
        "popular",
        qbz_i18n::mark("Popular Tracks"),
        !doc.top_tracks.is_empty(),
    );
    push(
        &mut tabs,
        "releases",
        qbz_i18n::mark("Releases"),
        !doc.releases.is_empty(),
    );
    push(
        &mut tabs,
        "critics",
        qbz_i18n::mark("Critics' Picks"),
        !doc.critics.is_empty(),
    );
    push(
        &mut tabs,
        "playlists",
        qbz_i18n::mark("Playlists"),
        !doc.playlists.is_empty(),
    );
    push(
        &mut tabs,
        "artists",
        qbz_i18n::mark("Artists"),
        !doc.artists.is_empty(),
    );
    push(
        &mut tabs,
        "labels",
        qbz_i18n::mark("More Labels"),
        !doc.more_labels.is_empty(),
    );
    tabs
}

// ===========================================================================
//  Parsing helpers (ported from label.rs — the Svelte getX helpers)
// ===========================================================================

/// The user's favorite-label ids, for the header Follow state and the
/// More-Labels cards.
///
/// PLURAL. `/favorite/getUserFavorites` takes `type` from a closed list and
/// answers 400 otherwise: `Invalid argument: type (accepted values are albums,
/// tracks, artists, articles, labels, awards)` — `qbz-qobuz` passes `fav_type`
/// straight into that param (client.rs `get_favorites`). This asked for
/// "label" and 400'd on EVERY label-page open (observed in qbz.log,
/// 2026-07-29), which returned an empty set, seeded `is_following: false` even
/// for labels the user follows, and left `toggle_follow` permanently on its
/// add branch — there was no way to unfollow a label from a freshly opened
/// page. The parser two lines down was already reading the plural envelope key
/// `labels`, which is the tell.
///
/// (The singular spelling belongs to `/favorite/create|delete`, whose param is
/// `label_ids` — see `toggle_follow`. Same word, two endpoints, two
/// conventions.)
///
/// The result also refreshes `fav_cache_qt`, and the cache is the fallback
/// when the fetch fails or the session is offline — otherwise a network blip
/// still produces the "cannot unfollow" state described above.
async fn favorite_label_ids(
    runtime: &std::sync::Arc<qbz_app::shell::AppRuntime<qbz_core::LoggingAdapter>>,
) -> HashSet<u64> {
    match runtime.core().get_favorites("labels", 500, 0).await {
        Ok(v) => {
            let ids: HashSet<u64> = v
                .get("labels")
                .and_then(|l| l.get("items"))
                .and_then(|i| i.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|it| it.get("id").and_then(|x| x.as_u64()))
                        .collect()
                })
                .unwrap_or_default();
            // ONE page (limit 500, offset 0 — the reference's shape). Mirror
            // it as a full replace only when it clearly IS the whole set; a
            // brim-full page may have a second one behind it and would wipe
            // what the properly-paged shell warm put there.
            if ids.len() < 500 {
                let mirror = ids.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    crate::fav_cache_qt::set_all_labels(mirror)
                })
                .await;
            }
            ids
        }
        Err(e) => {
            log::warn!("[qbz-qt] favorite labels fetch failed, falling back to the cache: {e}");
            crate::fav_cache_qt::all_labels()
        }
    }
}

/// `/label/page`'s flexible `image` value: a bare string, else
/// mega|extralarge|large|thumbnail|small (label.rs:143-156).
pub(crate) fn extract_label_image(image: Option<&Value>) -> String {
    let Some(image) = image else {
        return String::new();
    };
    if let Some(s) = image.as_str() {
        return s.to_string();
    }
    for key in ["mega", "extralarge", "large", "thumbnail", "small"] {
        if let Some(s) = image.get(key).and_then(|v| v.as_str()) {
            return s.to_string();
        }
    }
    String::new()
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// `{display}` object form or a bare string.
fn name_display(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    v.get("display")
        .and_then(|d| d.as_str())
        .unwrap_or_default()
        .to_string()
}

fn parse_image_value(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    for key in ["large", "extralarge", "medium", "thumbnail", "small"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            return s.to_string();
        }
    }
    String::new()
}

fn parse_artist_image(raw: &Value) -> String {
    if let Some(image) = raw.get("image") {
        let url = parse_image_value(image);
        if !url.is_empty() {
            return url;
        }
    }
    if let Some(pic) = raw.get("picture").and_then(|v| v.as_str()) {
        if !pic.is_empty() {
            return pic.to_string();
        }
    }
    if let Some(portrait) = raw.get("images").and_then(|i| i.get("portrait")) {
        if let (Some(hash), Some(format)) = (
            portrait.get("hash").and_then(|v| v.as_str()),
            portrait.get("format").and_then(|v| v.as_str()),
        ) {
            return format!(
                "https://static.qobuz.com/images/artists/covers/medium/{hash}.{format}"
            );
        }
    }
    String::new()
}

fn parse_playlist_image(raw: &Value) -> String {
    if let Some(image) = raw.get("image") {
        if let Some(rect) = image.get("rectangle").and_then(|v| v.as_str()) {
            if !rect.is_empty() {
                return rect.to_string();
            }
        }
        if let Some(cover) = image
            .get("covers")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
        {
            return cover.to_string();
        }
        for key in ["large", "thumbnail", "small"] {
            if let Some(s) = image.get(key).and_then(|v| v.as_str()) {
                return s.to_string();
            }
        }
    }
    for key in ["images300", "images150", "images"] {
        if let Some(s) = raw
            .get(key)
            .and_then(|a| a.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
        {
            return s.to_string();
        }
    }
    String::new()
}

fn parse_explore_image(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    for key in ["large", "thumbnail", "small"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            return s.to_string();
        }
    }
    String::new()
}

fn mmss(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// One Popular-Tracks row (label.rs `parse_top_track`).
///
/// The label `/page` top_tracks carry the main artist in a TRACK-level
/// `artists` array (roles = "main-artist"), NOT in `performer`/`artist` —
/// those are null here (qbz discussion #631).
fn parse_top_track(index: usize, raw: &Value) -> TrackRow {
    let id = raw
        .get("id")
        .and_then(|v| v.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_default();
    let title = raw
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let duration = raw.get("duration").and_then(|v| v.as_u64()).unwrap_or(0);
    let album = raw.get("album");
    let album_id = album
        .and_then(|a| a.get("id"))
        .map(value_to_string)
        .unwrap_or_default();
    let album_title = album
        .and_then(|a| a.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let artwork_url = album
        .and_then(|a| a.get("image"))
        .map(parse_image_value)
        .unwrap_or_default();
    let main_artist = raw
        .get("artists")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|a| {
                    a.get("roles")
                        .and_then(|r| r.as_array())
                        .map(|roles| roles.iter().any(|x| x.as_str() == Some("main-artist")))
                        .unwrap_or(false)
                })
                .or_else(|| arr.first())
        })
        .or_else(|| raw.get("performer"))
        .or_else(|| raw.get("artist"))
        .or_else(|| album.and_then(|a| a.get("artist")));
    let artist = main_artist
        .and_then(|p| p.get("name"))
        .map(name_display)
        .unwrap_or_default();
    let artist_id = main_artist
        .and_then(|p| p.get("id"))
        .map(value_to_string)
        .unwrap_or_default();
    let bit_depth = raw
        .get("audio_info")
        .and_then(|a| a.get("maximum_bit_depth"))
        .and_then(|v| v.as_u64())
        .or_else(|| raw.get("maximum_bit_depth").and_then(|v| v.as_u64()))
        .map(|b| b as u32);
    let sample_rate = raw
        .get("audio_info")
        .and_then(|a| a.get("maximum_sampling_rate"))
        .and_then(|v| v.as_f64())
        .or_else(|| raw.get("maximum_sampling_rate").and_then(|v| v.as_f64()));
    // Parsed from raw JSON, so the key is read directly, with the nested
    // `rights` object as a fallback — the label payload is the least documented
    // of the three track shapes and Qobuz uses both spellings elsewhere.
    // `unwrap_or(true)` is §3.1 spelled out at the parse site: a missing key
    // means the endpoint was terse, never that the track is dead.
    let not_streamable = !raw
        .get("streamable")
        .and_then(|v| v.as_bool())
        .or_else(|| {
            raw.get("rights")
                .and_then(|r| r.get("streamable"))
                .and_then(|v| v.as_bool())
        })
        .unwrap_or(true);
    // Same rule as `qbz_models::Track::is_upcoming`, read off the raw keys.
    let upcoming = not_streamable
        && qbz_models::types::upcoming_now(
            raw.get("streamable_at").and_then(|v| v.as_i64()),
            raw.get("release_date_stream").and_then(|v| v.as_str()),
        );
    TrackRow {
        featured: Vec::new(),
        is_favorite: id
            .parse::<u64>()
            .map(crate::fav_cache_qt::contains_track)
            .unwrap_or(false),
        cache_status: if crate::offline_qt::is_cached(&id) {
            3
        } else {
            0
        },
        id,
        number: (index + 1).to_string(),
        title,
        artist,
        artist_id,
        album: album_title,
        album_id,
        duration: mmss(duration as u32),
        quality_tier: crate::home_qt::quality_tier_from_depth(bit_depth).to_string(),
        quality_detail: crate::home_qt::quality_detail_from_parts(bit_depth, sample_rate),
        // Same contract as the album / artist rows (`TrackRow::bit_depth`):
        // the RAW numbers ride with the row so `track_row_to_queue` can fill
        // the queue entry instead of hardcoding `None`.
        bit_depth,
        sample_rate,
        explicit: raw
            .get("parental_warning")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        disc: 1,
        work_header: String::new(),
        work_composer_name: String::new(),
        work_composer_id: String::new(),
        artwork_url,
        not_streamable,
        upcoming,
    }
}

fn parse_playlist(raw: &Value) -> HomeCard {
    let owner = raw
        .get("owner")
        .and_then(|o| o.get("name"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Qobuz")
        .to_string();
    let track_count = raw
        .get("tracks_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let id = raw.get("id").map(value_to_string).unwrap_or_default();
    let pid = id.parse::<u64>().ok();
    HomeCard {
        // Pin badge state from the per-user store, like every other card
        // row: without it PlaylistCard draws the hollow glyph on a playlist
        // that IS pinned and the first click un-pins it.
        is_pinned: crate::sidebar_qt::is_pinned("playlist", &id),
        // Heart + the ownership tri-state. `/label/page` playlists arrive as
        // raw JSON with no owner block worth trusting, so ownership is
        // resolved by ID against the user's own playlist list rather than by
        // comparing an owner field that may not be there.
        is_favorite: pid
            .map(crate::fav_cache_qt::is_playlist_favorite)
            .unwrap_or(false),
        playlist_owned: pid.map(crate::playlist_qt::is_owned).unwrap_or(false),
        playlist_following: pid.map(crate::playlist_qt::is_following).unwrap_or(false),
        id,
        title: raw
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        subtitle: format!("{owner} · {track_count}"),
        art_url: parse_playlist_image(raw),
        ..HomeCard::default()
    }
}

fn parse_artist(raw: &Value) -> HomeCard {
    let id = raw.get("id").map(value_to_string).unwrap_or_default();
    HomeCard {
        // Same seed as the playlist rows above (ArtistCard's glyph).
        is_pinned: crate::sidebar_qt::is_pinned("artist", &id),
        is_favorite: id
            .parse::<u64>()
            .map(crate::fav_cache_qt::is_artist_favorite)
            .unwrap_or(false),
        id,
        title: raw.get("name").map(name_display).unwrap_or_default(),
        item_kind: "artist".to_string(),
        art_url: parse_artist_image(raw),
        ..HomeCard::default()
    }
}

fn parse_more_labels(resp: &qbz_models::LabelExploreResponse, current: u64) -> Vec<HomeCard> {
    let Some(items) = resp.items.as_ref() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(|x| x.as_u64())?;
            if id == current {
                return None;
            }
            Some(HomeCard {
                // "More labels" rows ARE followable entities — the follow set
                // is the same one the header reads (`favorite_label_ids` /
                // `fav_cache_qt`), so a label the user already follows draws
                // the followed state here too.
                is_favorite: crate::fav_cache_qt::is_label_favorite(id),
                id: id.to_string(),
                title: item
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string(),
                item_kind: "label".to_string(),
                art_url: item
                    .get("image")
                    .map(parse_explore_image)
                    .unwrap_or_default(),
                ..HomeCard::default()
            })
        })
        .collect()
}

/// `TrackRow` -> `QueueTrack` (the `artist_qt::track_row_to_queue` shape).
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
        // playback.rs `make_queue_track` (:2426): the CATALOG max travels with
        // the queue track. `None` here zeroed `quality_state`'s `TRACK_MAX_*`
        // seed, so a label Popular-Tracks play drew a bare tier on the NPB
        // AudioStamp with no "24-bit / 96 kHz" line.
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
        // D5: carried from the row (see `parse_top_track`). Hardcoding `true`
        // here discarded an answer the label payload had already given us.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn card(title: &str, artist: &str, year: &str, tier: &str) -> HomeCard {
        HomeCard {
            title: title.to_string(),
            artist: artist.to_string(),
            year: year.to_string(),
            release_sort_key: qbz_text_utils::dates::release_sort_key(Some(year)),
            quality_tier: tier.to_string(),
            ..HomeCard::default()
        }
    }

    fn state_with(albums: Vec<HomeCard>) -> LabelState {
        let mut s = LabelState::new();
        s.albums = albums;
        s.sort_by = "newest".to_string();
        s
    }

    #[test]
    fn extract_label_image_bare_string_and_object() {
        assert_eq!(extract_label_image(None), "");
        let bare = serde_json::json!("https://x/y.jpg");
        assert_eq!(extract_label_image(Some(&bare)), "https://x/y.jpg");
        let obj = serde_json::json!({ "large": "L", "mega": "M" });
        // mega wins (the Svelte extraction order).
        assert_eq!(extract_label_image(Some(&obj)), "M");
    }

    #[test]
    fn hires_count_is_over_the_full_catalog_not_the_filter() {
        let mut s = state_with(vec![
            card("A", "X", "2020", "hires"),
            card("B", "Y", "2019", "cd"),
        ]);
        s.query = "b".to_string();
        let (visible, _grouped, shown, hires) = derive_releases(&s);
        assert_eq!(shown, 1);
        assert_eq!(visible.len(), 1);
        // The filter kept the CD album, but the count still reports the
        // catalog's one Hi-Res title (label.rs:190-193).
        assert_eq!(hires, 1);
    }

    #[test]
    fn fast_path_returns_the_catalog_untouched() {
        let s = state_with(vec![card("A", "X", "2020", "cd")]);
        let (visible, grouped, shown, _) = derive_releases(&s);
        assert_eq!(visible.len(), 1);
        assert!(grouped.is_empty());
        assert_eq!(shown, 1);
    }

    #[test]
    fn group_by_artist_buckets_in_first_appearance_order() {
        let mut s = state_with(vec![
            card("A", "Zeta", "2020", "cd"),
            card("B", "Alpha", "2019", "cd"),
            card("C", "Zeta", "2018", "cd"),
        ]);
        s.group_by = true;
        s.sort_by = "artist-asc".to_string();
        let (visible, grouped, shown, _) = derive_releases(&s);
        assert!(visible.is_empty());
        assert_eq!(shown, 3);
        assert_eq!(grouped.len(), 2);
        // artist-asc sorted first, so Alpha appears before Zeta.
        assert_eq!(grouped[0].title, "Alpha");
        assert_eq!(grouped[1].albums.len(), 2);
    }

    #[test]
    fn sorting_uses_the_plain_year() {
        let mut items = vec![card("A", "X", "1999", "cd"), card("B", "Y", "2021", "cd")];
        sort_cards(&mut items, "newest");
        assert_eq!(items[0].year, "2021");
        sort_cards(&mut items, "oldest");
        assert_eq!(items[0].year, "1999");
        sort_cards(&mut items, "title-desc");
        assert_eq!(items[0].title, "B");
    }

    #[test]
    fn jump_tabs_only_list_non_empty_sections() {
        let mut doc = LabelDoc::default();
        let tabs = build_jump_tabs(&doc);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].id, "about");
        doc.releases.push(card("A", "X", "2020", "cd"));
        doc.more_labels.push(card("L", "", "", ""));
        let tabs = build_jump_tabs(&doc);
        let ids: Vec<&str> = tabs.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["about", "releases", "labels"]);
    }
}
