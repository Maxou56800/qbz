//! Award landing page + "See all" listing — the Slint-free port of
//! `crates/qbz/src/award.rs`.
//!
//! ONE module for BOTH routes, exactly like `label_qt.rs` (which is the
//! precedent this whole feature follows): they share the hero, the follow
//! state and the award identity, so splitting them would duplicate all three.
//!
//! Two documents on the bridge:
//!   `awardJson`        -> `qml/views/AwardView.qml`        (route "award")
//!   `awardAlbumsJson`  -> `qml/views/AwardAlbumsView.qml`  ("awardalbums")
//!
//! # The three rules carried from the Tauri review — do NOT re-litigate
//!
//! 1. **The grid is ALWAYS `/award/getAlbums`.** `/award/page` is user-scoped
//!    and may omit releases entirely; it is read for the hero (name, image,
//!    magazine) and nothing else. Never map `page.releases`.
//! 2. **The favorite split is plural-read / singular-write.** Reads go through
//!    `get_favorites("awards")`, writes through `add/remove_favorite("award")`
//!    — the backend builds `format!("{}_ids")` from the write kind, so
//!    `award` + `s` is `award_ids`. Unifying the two silently breaks
//!    favouriting while reads keep working, which is the worst shape of bug.
//! 3. **`get_award_albums` has no `has_more`.** Its `total` is a client HINT
//!    (`offset + len + has_more?1`), so the real predicate is
//!    `total > loaded` — used directly, which is also what fixes the
//!    exactly-one-page See-All edge.
//!
//! # The name -> id resolver
//!
//! Qobuz omits the award id on some `/album/get` entries, so the album
//! sidebar can only offer a NAME. `resolve_award_id_by_name` answers from a
//! process-local normalized cache — harvested by [`remember_awards`] as album
//! documents load — and falls back to crawling `/award/explore`.
//!
//! # The award CATALOG (this port only)
//!
//! The explore crawl also feeds a full id+name catalog, published with the
//! landing document so the view can offer a dropdown to ANY award without
//! having to find an album that won it first. The Slint has no such control;
//! it is the one addition in this port, and it costs nothing extra — the
//! crawl already existed for the resolver.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use cxx_qt_lib::QString;
use serde::Serialize;
use serde_json::Value;

use crate::home_bridge;
use crate::home_qt::HomeCard;

/// Landing preview grid page size (Tauri AwardView PAGE_SIZE).
const PREVIEW_PAGE_SIZE: u32 = 20;
/// Full-listing page size (Tauri AwardAlbumsView PAGE_SIZE).
const LIST_PAGE_SIZE: u32 = 50;
/// "Other awards" carousel size (Tauri AwardView loadOtherAwards limit).
const OTHER_AWARDS_LIMIT: u32 = 30;
/// `/award/explore` crawl page size + page cap (Tauri awardCatalogStore).
const EXPLORE_PAGE_SIZE: u32 = 100;
const EXPLORE_MAX_PAGES: u32 = 40;

// ===========================================================================
//  Name -> id catalog (process-local, harvested + explore-crawled)
// ===========================================================================

/// Normalized award name -> (id, display name). The display name is kept
/// because the dropdown lists it; the key exists only to match.
static CATALOG: Mutex<Option<HashMap<String, (String, String)>>> = Mutex::new(None);

/// Lowercase + trim + collapse whitespace. Both sides of every comparison come
/// from the SAME Qobuz field, so this is enough for self-matching (Tauri also
/// strips diacritics, which is moot when the strings share a source).
fn normalize(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Harvest `(id, name)` pairs into the catalog. Called by `album_qt` for every
/// album document that carries awards, so the ids an album DOES name are known
/// before anyone asks about the ones it does not.
pub fn remember_awards(pairs: &[(String, String)]) {
    if pairs.is_empty() {
        return;
    }
    let Ok(mut guard) = CATALOG.lock() else {
        return;
    };
    let map = guard.get_or_insert_with(HashMap::new);
    for (id, name) in pairs {
        if id.trim().is_empty() || name.trim().is_empty() {
            continue;
        }
        map.insert(normalize(name), (id.clone(), name.clone()));
    }
}

fn lookup_cached(name: &str) -> Option<String> {
    let key = normalize(name);
    CATALOG
        .lock()
        .ok()?
        .as_ref()?
        .get(&key)
        .map(|(id, _)| id.clone())
}

/// The catalog as a NAME-sorted list, for the dropdown.
fn catalog_entries() -> Vec<(String, String)> {
    let Ok(guard) = CATALOG.lock() else {
        return Vec::new();
    };
    let Some(map) = guard.as_ref() else {
        return Vec::new();
    };
    // Keyed by NORMALIZED NAME, so one award reached under two spellings (the
    // album's and explore's) is two entries with ONE id. Deduped by id here,
    // keeping the longest spelling — the fuller name is the more useful row.
    //
    // This is also the belt for the blank row the owner saw in the dropdown
    // (2026-08-16): whatever produced a title-less entry, an untitled row can
    // no longer reach the list, and its id keeps whichever sibling HAS a name.
    let mut best: HashMap<String, String> = HashMap::new();
    for (id, name) in map.values() {
        let name = name.trim();
        if id.trim().is_empty() || name.is_empty() {
            // Loud on purpose. The owner saw a TITLE-LESS row in the dropdown
            // once (2026-08-16) and it did not reproduce, so the mechanism is
            // still unknown — and the drop above is exactly what would hide it
            // next time. If this line ever prints, the id it names is the
            // whole answer: cross it against `/award/explore` and the album
            // that harvested it.
            log::warn!(
                "[qbz-qt] award catalog: dropping untitled entry (id={id:?}, name={name:?})"
            );
            continue;
        }
        best.entry(id.clone())
            .and_modify(|cur| {
                if name.len() > cur.len() {
                    *cur = name.to_string();
                }
            })
            .or_insert_with(|| name.to_string());
    }
    let mut out: Vec<(String, String)> = best.into_iter().collect();
    out.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    out
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// One `/award/explore` page: harvest it into the catalog and hand back the
/// raw items so a caller can also read magazine / image off them.
async fn explore_page(limit: u32, offset: u32) -> (Vec<Value>, bool) {
    let runtime = crate::app();
    let resp = match runtime.core().get_award_explore(limit, offset).await {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[qbz-qt] award explore ({offset}) failed: {e}");
            return (Vec::new(), false);
        }
    };
    let items = resp
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let pairs: Vec<(String, String)> = items
        .iter()
        .filter_map(|it| {
            let id = it.get("id").map(value_to_string)?;
            let name = it.get("name").and_then(|v| v.as_str())?.to_string();
            (!id.is_empty() && !name.is_empty()).then_some((id, name))
        })
        .collect();
    remember_awards(&pairs);
    let has_more = resp
        .get("has_more")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    (items, has_more)
}

/// Resolve an award NAME to its id: the cache first, then an `/award/explore`
/// crawl. `None` means the crawl finished without a match — the caller toasts
/// rather than opening a page that cannot load.
pub async fn resolve_award_id_by_name(name: &str) -> Option<String> {
    if name.trim().is_empty() {
        return None;
    }
    if let Some(id) = lookup_cached(name) {
        return Some(id);
    }
    let target = normalize(name);
    let mut offset = 0u32;
    let mut seen: HashSet<String> = HashSet::new();
    for _ in 0..EXPLORE_MAX_PAGES {
        let (items, has_more) = explore_page(EXPLORE_PAGE_SIZE, offset).await;
        if items.is_empty() {
            break;
        }
        let mut new_ids = 0usize;
        for it in &items {
            let Some(id) = it.get("id").map(value_to_string) else {
                continue;
            };
            let nm = it.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            if seen.insert(id.clone()) {
                new_ids += 1;
            }
            if !nm.is_empty() && normalize(nm) == target {
                return Some(id);
            }
        }
        offset += items.len() as u32;
        // `new_ids == 0` guards a server that keeps answering with the same
        // page: without it the crawl spends all 40 requests re-reading it.
        if !has_more || new_ids == 0 {
            break;
        }
    }
    log::warn!("[qbz-qt] award id unresolved for name: {name}");
    None
}

/// Fill the catalog in the background so the view's dropdown can list every
/// award. Runs at most ONCE per session: the crawl is up to 40 requests, and
/// the set it reads does not change while the app is open.
fn ensure_catalog_crawled() {
    static CRAWLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if CRAWLED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    crate::spawn(async move {
        let mut offset = 0u32;
        let mut seen = 0usize;
        for _ in 0..EXPLORE_MAX_PAGES {
            let (items, has_more) = explore_page(EXPLORE_PAGE_SIZE, offset).await;
            if items.is_empty() {
                break;
            }
            offset += items.len() as u32;
            seen += items.len();
            if !has_more {
                break;
            }
        }
        log::info!("[qbz-qt] award catalog crawled: {seen} award(s)");
        // Republish so a page already on screen gains its dropdown without
        // the user having to leave and come back.
        publish_award();
    });
}

// ===========================================================================
//  Documents
// ===========================================================================

#[derive(Serialize, Clone, Default)]
struct OtherAward {
    id: String,
    /// `title` (not `name`): the carousel delegate is the shared card, whose
    /// model field is `title` — the same shape the label page's "More labels"
    /// rail feeds.
    title: String,
    artist: String,
    #[serde(rename = "imageUrl")]
    image_url: String,
}

#[derive(Serialize, Clone, Default)]
struct CatalogEntry {
    id: String,
    name: String,
}

#[derive(Serialize, Clone, Default)]
struct AwardDoc {
    id: String,
    name: String,
    #[serde(rename = "imageUrl")]
    image_url: String,
    #[serde(rename = "magazineName")]
    magazine_name: String,
    #[serde(rename = "isFollowing")]
    is_following: bool,
    #[serde(rename = "followToggling")]
    follow_toggling: bool,
    albums: Vec<HomeCard>,
    /// The server's client-side HINT, not a count we can trust for anything
    /// except "is there more" (rule 3 in the module header).
    total: i64,
    #[serde(rename = "hasMore")]
    has_more: bool,
    #[serde(rename = "loadError")]
    load_error: bool,
    /// True while a load-more page is in flight (the landing has its own
    /// button now, so it needs the same flag the listing had).
    #[serde(rename = "loadingMore")]
    loading_more: bool,
    #[serde(rename = "searchQuery")]
    search_query: String,
    #[serde(rename = "otherAwards")]
    other_awards: Vec<OtherAward>,
    /// EVERY award, name-sorted — the dropdown's model. Empty until the
    /// background crawl lands; the view hides the control while it is.
    catalog: Vec<CatalogEntry>,
    /// Index of the OPEN award inside `catalog`, or -1 when the crawl has not
    /// reached it. The dropdown binds its `currentIndex` to this.
    #[serde(rename = "catalogIndex")]
    catalog_index: i32,
}

#[derive(Serialize, Clone, Default)]
struct AwardAlbumsDoc {
    id: String,
    name: String,
    /// The SEARCH-FILTERED list the grid renders.
    albums: Vec<HomeCard>,
    total: i64,
    #[serde(rename = "hasMore")]
    has_more: bool,
    #[serde(rename = "loadError")]
    load_error: bool,
    #[serde(rename = "loadingMore")]
    loading_more: bool,
    #[serde(rename = "searchQuery")]
    search_query: String,
}

// ===========================================================================
//  State
// ===========================================================================

#[derive(Default)]
struct State {
    /// Bumped by every open; a late reply whose generation is stale is dropped
    /// rather than painted over the page the user is now on.
    generation: u64,
    id: String,
    name: String,
    image_url: String,
    magazine_name: String,
    is_following: bool,
    follow_toggling: bool,
    other_awards: Vec<OtherAward>,
    /// EVERY album loaded so far, unfiltered — the listing paginates into this
    /// and the search filters a view of it.
    all_albums: Vec<HomeCard>,
    total: i64,
    loading: bool,
    load_error: bool,
    /// Listing sub-view only.
    albums_loading_more: bool,
    search_query: String,
}

static AWARD: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = AWARD.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(State::default))
}

/// Real `has_more`: the server's `total` is a hint built as
/// `offset + len + (has_more ? 1 : 0)`, so the only sound predicate is
/// "the hint is bigger than what we hold" (rule 3).
fn has_more_of(total: i64, loaded: usize) -> bool {
    total > loaded as i64
}

// ===========================================================================
//  Publish
// ===========================================================================

/// The loaded set filtered by the current query. ONE filter for both views:
/// they are the same award and the same accumulating list, so a query typed
/// on the landing is still in force behind "See All".
fn visible_albums(s: &State) -> Vec<HomeCard> {
    let q = s.search_query.trim().to_lowercase();
    if q.is_empty() {
        return s.all_albums.clone();
    }
    s.all_albums
        .iter()
        .filter(|c| c.title.to_lowercase().contains(&q) || c.artist.to_lowercase().contains(&q))
        .cloned()
        .collect()
}

fn publish_award() {
    let (doc, loading) = with_state(|s| {
        let catalog = catalog_entries();
        let catalog_index = catalog
            .iter()
            .position(|(id, _)| *id == s.id)
            .map(|i| i as i32)
            .unwrap_or(-1);
        (
            AwardDoc {
                id: s.id.clone(),
                name: s.name.clone(),
                image_url: s.image_url.clone(),
                magazine_name: s.magazine_name.clone(),
                is_following: s.is_following,
                follow_toggling: s.follow_toggling,
                // NOT capped at PREVIEW_PAGE_SIZE any more. The first fetch
                // still asks for one preview page so the first paint is
                // cheap, but the landing has its own Load more now — capping
                // the document would have made that button load rows the view
                // then threw away.
                albums: visible_albums(s),
                total: s.total,
                has_more: has_more_of(s.total, s.all_albums.len()),
                load_error: s.load_error,
                loading_more: s.albums_loading_more,
                search_query: s.search_query.clone(),
                other_awards: s.other_awards.clone(),
                catalog: catalog
                    .into_iter()
                    .map(|(id, name)| CatalogEntry { id, name })
                    .collect(),
                catalog_index,
            },
            s.loading,
        )
    });
    let json = serde_json::to_string(&doc).unwrap_or_else(|_| "{}".into());
    home_bridge::ui(move |mut b| {
        b.as_mut().set_award_json(QString::from(json.as_str()));
        b.as_mut().set_award_loading(loading);
    });
}

fn publish_albums() {
    let (doc, loading) = with_state(|s| {
        (
            AwardAlbumsDoc {
                id: s.id.clone(),
                name: s.name.clone(),
                albums: visible_albums(s),
                total: s.total,
                has_more: has_more_of(s.total, s.all_albums.len()),
                load_error: s.load_error,
                loading_more: s.albums_loading_more,
                search_query: s.search_query.clone(),
            },
            s.loading,
        )
    });
    let json = serde_json::to_string(&doc).unwrap_or_else(|_| "{}".into());
    home_bridge::ui(move |mut b| {
        b.as_mut()
            .set_award_albums_json(QString::from(json.as_str()));
        b.as_mut().set_award_albums_loading(loading);
    });
}

fn publish_both() {
    publish_award();
    publish_albums();
}

// ===========================================================================
//  Landing
// ===========================================================================

/// Album sidebar laurel / "Other awards" card / the dropdown: open the award
/// page. `fallback_name` paints the hero until `/award/page` answers (and is
/// the whole hero when that endpoint omits the award, which it may).
pub fn open_award(award_id: String, fallback_name: String) {
    if crate::offline_fwd::engine().status().is_offline() {
        return;
    }
    let id = award_id.trim().to_string();
    if id.is_empty() {
        return;
    }
    crate::nav_qt::record_with("award", serde_json::json!({ "id": &id, "name": &fallback_name }));
    let generation = with_state(|s| {
        s.generation = s.generation.wrapping_add(1);
        s.id = id.clone();
        s.name = fallback_name.clone();
        s.image_url.clear();
        s.magazine_name.clear();
        s.is_following = false;
        s.follow_toggling = false;
        s.other_awards.clear();
        s.all_albums.clear();
        s.total = 0;
        s.load_error = false;
        s.albums_loading_more = false;
        s.search_query.clear();
        s.loading = true;
        s.generation
    });
    publish_both();
    // The dropdown's model. Fire-and-forget and once per session.
    ensure_catalog_crawled();
    fetch_page(generation, id, PREVIEW_PAGE_SIZE);
}

/// The album sidebar's other arm: Qobuz gave a name but no id. Resolve, then
/// open — or toast, because opening a page with no id shows nothing and
/// explains nothing.
pub fn open_award_by_name(name: String) {
    if crate::offline_fwd::engine().status().is_offline() {
        return;
    }
    if name.trim().is_empty() {
        return;
    }
    crate::spawn(async move {
        match resolve_award_id_by_name(&name).await {
            Some(id) => open_award(id, name),
            // Untranslated, 1:1 with the reference (`main.rs:13298-13301`
            // hands `toast::info_weak` a raw literal). Not a msgid in any of
            // the eight catalogs, and inventing one here would be a string
            // only this port has.
            None => crate::toast_qt::info("Award detail not available for this entry."),
        }
    });
}

/// The error branch's Retry.
pub fn retry() {
    let (id, name) = with_state(|s| (s.id.clone(), s.name.clone()));
    if !id.is_empty() {
        open_award(id, name);
    }
}

fn fetch_page(generation: u64, id: String, limit: u32) {
    let runtime = crate::app();
    crate::spawn(async move {
        // --- Hero. Best-effort: it must never block or fail the grid, which
        // is the half the page is actually about (Tauri loads them
        // independently for the same reason). `AwardPageData` is TYPED and
        // every field is `Option` — this endpoint is user-scoped and answers
        // with holes, which is exactly why it is not allowed near the grid.
        if let Ok(page) = runtime.core().get_award_page(&id).await {
            let name = page.name.clone().unwrap_or_default();
            let image_url = page.image.clone().unwrap_or_default();
            let magazine = page
                .magazine
                .as_ref()
                .and_then(|m| m.name.clone())
                .unwrap_or_default();
            with_state(|s| {
                if s.generation != generation {
                    return;
                }
                if !name.is_empty() {
                    s.name = name;
                }
                s.image_url = image_url;
                s.magazine_name = magazine;
            });
            publish_award();
        }

        // --- Follow state (rule 2: read PLURAL).
        let following = favorite_award_ids().await.contains(&id);
        with_state(|s| {
            if s.generation == generation {
                s.is_following = following;
            }
        });

        // --- The grid. ALWAYS /award/getAlbums (rule 1).
        match runtime.core().get_award_albums(&id, limit, 0).await {
            Ok(page) => {
                let mut cards: Vec<HomeCard> = page
                    .items
                    .into_iter()
                    .map(crate::home_qt::map_flat_album)
                    .collect();
                // A card carries a cover URL, not a cover: `artPath` is filled
                // from the on-disk cache here and the misses are downloaded
                // below. Without this the grid draws EMPTY TILES forever —
                // there is no lazy fetch behind AlbumCollection, the path has
                // to be in the document (the `attach_doc_art` rule in
                // label_qt.rs:442).
                let missing = crate::home_qt::attach_card_art(&mut cards);
                let total = page.total as i64;
                with_state(|s| {
                    if s.generation != generation {
                        return;
                    }
                    s.all_albums = cards;
                    s.total = total;
                    s.load_error = false;
                    s.loading = false;
                });
                publish_both();
                fill_missing_art(generation, missing).await;
            }
            Err(e) => {
                log::warn!("[qbz-qt] award albums load failed ({id}): {e}");
                with_state(|s| {
                    if s.generation != generation {
                        return;
                    }
                    s.load_error = true;
                    s.loading = false;
                });
            }
        }
        publish_both();

        // --- "Other awards" rail. Its own request, after the page is usable.
        let (items, _) = explore_page(OTHER_AWARDS_LIMIT, 0).await;
        let others: Vec<OtherAward> = items
            .iter()
            .filter_map(|it| {
                let oid = it.get("id").map(value_to_string)?;
                let title = it.get("name").and_then(|v| v.as_str())?.to_string();
                if oid.is_empty() || oid == id || title.is_empty() {
                    return None;
                }
                Some(OtherAward {
                    id: oid,
                    title,
                    artist: it
                        .get("magazine")
                        .and_then(|m| m.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    image_url: it
                        .get("image")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                })
            })
            .collect();
        with_state(|s| {
            if s.generation == generation {
                s.other_awards = others;
            }
        });
        publish_award();
    });
}

// ===========================================================================
//  Follow (rule 2: read "awards", write "award")
// ===========================================================================

async fn favorite_award_ids() -> HashSet<String> {
    let runtime = crate::app();
    match runtime.core().get_favorites("awards", 500, 0).await {
        Ok(value) => value
            .get("awards")
            .and_then(|v| v.get("items"))
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|it| it.get("id").map(value_to_string))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        Err(e) => {
            log::warn!("[qbz-qt] award favorites read failed: {e}");
            HashSet::new()
        }
    }
}

pub fn toggle_follow() {
    let (id, following, busy) = with_state(|s| (s.id.clone(), s.is_following, s.follow_toggling));
    if id.is_empty() || busy {
        return;
    }
    with_state(|s| s.follow_toggling = true);
    publish_award();
    let runtime = crate::app();
    crate::spawn(async move {
        // SINGULAR on the write — the backend derives `award_ids` from it.
        let result = if following {
            runtime.core().remove_favorite("award", &id).await
        } else {
            runtime.core().add_favorite("award", &id).await
        };
        match result {
            Ok(_) => with_state(|s| {
                if s.id == id {
                    s.is_following = !following;
                }
                s.follow_toggling = false;
            }),
            Err(e) => {
                log::warn!("[qbz-qt] award follow toggle failed ({id}): {e}");
                with_state(|s| s.follow_toggling = false);
            }
        }
        publish_award();
    });
}

// ===========================================================================
//  Listing sub-view ("See all")
// ===========================================================================

/// "See All": push the listing route and load its FIRST page at the listing
/// size. The preview already holds 20; the listing wants 50, so it reloads
/// from offset 0 rather than paginating on top of a differently-sized page.
pub fn open_albums() {
    let (id, name) = with_state(|s| (s.id.clone(), s.name.clone()));
    if id.is_empty() {
        return;
    }
    crate::nav_qt::record_with("awardalbums", serde_json::json!({ "id": &id, "name": &name }));
    let generation = with_state(|s| {
        s.generation = s.generation.wrapping_add(1);
        s.all_albums.clear();
        s.total = 0;
        s.load_error = false;
        s.search_query.clear();
        s.loading = true;
        s.generation
    });
    // Both: the reset clears the list the landing is showing too.
    publish_both();
    let _ = name;
    fetch_albums_page(generation, id, 0, true);
}

pub fn albums_load_more() {
    let (id, offset, busy, loading, more) = with_state(|s| {
        (
            s.id.clone(),
            s.all_albums.len() as u32,
            s.albums_loading_more,
            s.loading,
            has_more_of(s.total, s.all_albums.len()),
        )
    });
    if id.is_empty() || busy || loading || !more {
        return;
    }
    let generation = with_state(|s| {
        s.albums_loading_more = true;
        s.generation
    });
    publish_both();
    fetch_albums_page(generation, id, offset, false);
}

fn fetch_albums_page(generation: u64, id: String, offset: u32, replace: bool) {
    let runtime = crate::app();
    crate::spawn(async move {
        match runtime
            .core()
            .get_award_albums(&id, LIST_PAGE_SIZE, offset)
            .await
        {
            Ok(page) => {
                let mut cards: Vec<HomeCard> = page
                    .items
                    .into_iter()
                    .map(crate::home_qt::map_flat_album)
                    .collect();
                let missing = crate::home_qt::attach_card_art(&mut cards);
                let total = page.total as i64;
                with_state(|s| {
                    if s.generation != generation {
                        return;
                    }
                    if replace {
                        s.all_albums = cards;
                    } else {
                        s.all_albums.extend(cards);
                    }
                    s.total = total;
                    s.load_error = false;
                    s.loading = false;
                    s.albums_loading_more = false;
                });
                publish_both();
                fill_missing_art(generation, missing).await;
            }
            Err(e) => {
                log::warn!("[qbz-qt] award albums page failed ({id} @{offset}): {e}");
                with_state(|s| {
                    if s.generation != generation {
                        return;
                    }
                    // A failed LOAD-MORE keeps what is on screen: only the
                    // first page's failure is the error branch.
                    s.load_error = replace;
                    s.loading = false;
                    s.albums_loading_more = false;
                });
            }
        }
        publish_both();
    });
}

/// Download the covers a page was missing, re-attach every card's `artPath`
/// and republish. Generation-guarded at BOTH ends: the download is the long
/// part, and the user can have navigated to another award while it runs.
async fn fill_missing_art(generation: u64, missing: Vec<String>) {
    if missing.is_empty() {
        return;
    }
    crate::artwork_qt::download_missing(missing).await;
    let mut cards = with_state(|s| {
        if s.generation != generation {
            return Vec::new();
        }
        std::mem::take(&mut s.all_albums)
    });
    if cards.is_empty() {
        return;
    }
    let _ = crate::home_qt::attach_card_art(&mut cards);
    with_state(|s| {
        if s.generation == generation {
            s.all_albums = cards;
        }
    });
    publish_both();
}

/// Client-side filter over the loaded set — 1:1 with the reference, which
/// also never re-queries for the listing's search box. Republishes BOTH
/// documents: the landing grew its own box and they share the query.
pub fn albums_search(query: String) {
    with_state(|s| s.search_query = query);
    publish_both();
}

// No `republish_for_language` here on purpose. `label_qt`'s exists because it
// rebuilds JUMP TO tab labels that live INSIDE its document; these two carry
// no Rust-translated string at all — every label on both views is a `@tr` in
// QML, which `QbzSession.trRev` re-evaluates by itself. A republish hook that
// republishes nothing is the dead-code the rest of this module avoids.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_matches_the_same_string_from_both_sides() {
        assert_eq!(normalize("  Gramophone   Award "), "gramophone award");
        assert_eq!(normalize("GRAMOPHONE AWARD"), normalize("Gramophone Award"));
    }

    /// Rule 3. `total` is a HINT (`offset + len + has_more?1`), so the
    /// exactly-one-page case must NOT advertise more — that is the bug the
    /// direct comparison fixes.
    #[test]
    fn has_more_reads_the_hint_against_what_is_loaded() {
        assert!(has_more_of(21, 20));
        assert!(!has_more_of(20, 20));
        assert!(!has_more_of(0, 0));
        // A server that under-reports can never make the grid ask for a page
        // that does not exist.
        assert!(!has_more_of(5, 20));
    }

    #[test]
    fn the_catalog_harvests_and_resolves_by_normalized_name() {
        remember_awards(&[("42".into(), "  Album  Of The Year ".into())]);
        assert_eq!(lookup_cached("album of the year").as_deref(), Some("42"));
        assert_eq!(lookup_cached("ALBUM OF THE YEAR").as_deref(), Some("42"));
        assert!(lookup_cached("nothing like it").is_none());
        // The dropdown lists the DISPLAY name, not the normalized key.
        assert!(catalog_entries()
            .iter()
            .any(|(id, name)| id == "42" && name.contains("Album")));
    }

    #[test]
    fn empty_pairs_never_enter_the_catalog() {
        remember_awards(&[("".into(), "No id".into()), ("7".into(), "   ".into())]);
        assert!(lookup_cached("no id").is_none());
        assert!(!catalog_entries().iter().any(|(id, _)| id == "7"));
    }
}
