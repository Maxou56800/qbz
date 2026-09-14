//! The Discover "View all" full-list pages + the two local play-history
//! pages — the Slint-free port of `crates/qbz/src/discover_browse.rs`,
//! `playlist_browse.rs` and the `RecentAlbumsState` /
//! `MostPlayedAlbumsState` halves of `main.rs`.
//!
//! Four routes live here, all publishing ONE JSON document each onto
//! `QbzHome` (the phase-23 bridge pattern):
//!
//!   `discoverbrowse`    DiscoverBrowseView.slint  — one /discover/<module>
//!                       endpoint, infinite scroll (PAGE_SIZE 50), shared
//!                       genre filter, client-side search.
//!   `playlistbrowse`    PlaylistBrowseView.slint  — /discover/playlists,
//!                       server-side category tag + genre, client-side
//!                       search.
//!   `recentalbums`      RecentAlbumsView.slint    — the LOCAL play-history
//!                       album store (`recently_qt`, capped 24). No network.
//!   `mostplayedalbums`  MostPlayedAlbumsView.slint — the LOCAL play-count
//!                       store (`album_play_history::all_albums`, NOT the
//!                       rail's `top_albums(20)`), with a title/artist
//!                       filter.
//!
//! Split out of `home_qt.rs` deliberately: that module is already ~1.2 k
//! lines and these are four independent controllers, not more Home mapping.
//!
//! Known delta: the blacklist store is live, but the album drop the Slint
//! discover pages apply is not yet folded into these result lists. Playlists
//! are unfiltered in the reference too. Back/forward scroll restore is live
//! through `ScrollMemory.qml` + `nav_qt`.
//! - `nav_qt` stores view IDs, so back-to-`discoverbrowse` re-mounts
//!   whatever document is currently published (same limitation the album /
//!   artist routes already have).

use std::sync::Mutex;

use cxx_qt_lib::QString;

use crate::home_qt::{attach_card_art, HomeCard};

/// Page size for both catalog pages (discover_browse.rs:33 /
/// playlist_browse.rs:34).
pub const PAGE_SIZE: u32 = 50;

// ===========================================================================
//  Shared helpers
// ===========================================================================

/// Lowercase-contains over title + artist — the Slint `visible` derivation.
fn matches(card: &HomeCard, needle: &str) -> bool {
    needle.is_empty()
        || card.title.to_lowercase().contains(needle)
        || card.artist.to_lowercase().contains(needle)
}

/// JSON array of the cards a page should render (search applied).
fn visible_json(cards: &[HomeCard], query: &str) -> String {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return serde_json::to_string(cards).unwrap_or_else(|_| "[]".to_string());
    }
    let filtered: Vec<&HomeCard> = cards.iter().filter(|c| matches(c, &needle)).collect();
    serde_json::to_string(&filtered).unwrap_or_else(|_| "[]".to_string())
}

// ===========================================================================
//  DiscoverBrowse
// ===========================================================================

struct DiscoverBrowse {
    endpoint: String,
    title: String,
    /// Next page offset — advanced by the FETCHED count, never by the
    /// surviving count (discover_browse.rs:36-38: dropping rows client-side
    /// must not shift the server window).
    next_offset: u32,
    has_more: bool,
    cards: Vec<HomeCard>,
    query: String,
    /// "grid" | "list" — PERSISTS across navigations, like the Slint state
    /// global. Seeded to "grid" on the first open (an empty view-mode
    /// renders NOTHING: AlbumCollectionView.slint:294 tests `== "grid"`).
    view_mode: String,
    loading: bool,
    loading_more: bool,
    /// Bumped on every fresh open so a late page for the previous endpoint
    /// is dropped instead of appended (the artist_qt generation guard).
    generation: u64,
}

impl DiscoverBrowse {
    const fn new() -> Self {
        Self {
            endpoint: String::new(),
            title: String::new(),
            next_offset: 0,
            has_more: false,
            cards: Vec::new(),
            query: String::new(),
            view_mode: String::new(),
            loading: false,
            loading_more: false,
            generation: 0,
        }
    }
}

static DISCOVER: Mutex<DiscoverBrowse> = Mutex::new(DiscoverBrowse::new());

fn publish_discover() {
    let (json, loading, loading_more) = {
        let Ok(s) = DISCOVER.lock() else {
            return;
        };
        let doc = format!(
            r#"{{"title":{},"endpoint":{},"viewMode":{},"query":{},"hasMore":{},"total":{},"items":{}}}"#,
            json_str(&s.title),
            json_str(&s.endpoint),
            json_str(&s.view_mode),
            json_str(&s.query),
            s.has_more,
            s.cards.len(),
            visible_json(&s.cards, &s.query),
        );
        (doc, s.loading, s.loading_more)
    };
    crate::home_bridge::ui(move |mut b| {
        b.as_mut()
            .set_discover_browse_json(QString::from(json.as_str()));
        b.as_mut().set_discover_browse_loading(loading);
        b.as_mut().set_discover_browse_loading_more(loading_more);
    });
}

/// Minimal JSON string escaping for the hand-assembled documents above
/// (serde for the card arrays, this for the scalar fields).
fn json_str(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

/// Rail "View all" on a generic album carousel.
///
/// Order matters and mirrors `main.rs::open_album`: offline gate ->
/// `nav_qt::record` (which ALREADY publishes `currentView`, so there is no
/// second `set_current_view` call) -> clear the document + raise `loading`
/// in ONE bridge hop -> fetch. Without the clear the page renders the
/// PREVIOUS module until the fetch lands.
pub fn open_discover_browse(endpoint: String, title: String) {
    if crate::offline_fwd::engine().status().is_offline() {
        return;
    }
    if endpoint.is_empty() {
        log::warn!("[qbz-qt] open_discover_browse with an empty endpoint; ignoring");
        return;
    }
    crate::nav_qt::record_with(
        "discoverbrowse",
        serde_json::json!({ "endpoint": &endpoint, "title": &title }),
    );
    let generation = {
        let Ok(mut s) = DISCOVER.lock() else {
            return;
        };
        let mode = if s.view_mode.is_empty() {
            "grid".to_string()
        } else {
            s.view_mode.clone()
        };
        s.generation = s.generation.wrapping_add(1);
        s.endpoint = endpoint;
        s.title = title;
        s.next_offset = 0;
        s.has_more = false;
        s.cards.clear();
        s.query.clear();
        s.view_mode = mode;
        s.loading = true;
        s.loading_more = false;
        s.generation
    };
    publish_discover();
    fetch_discover(generation);
}

/// Infinite scroll. Guarded exactly like DiscoverBrowseView.slint:126-135 —
/// the search filter is client-side, so paging is disabled while one is
/// active (appending would bring back unfiltered rows).
pub fn discover_browse_load_more() {
    let generation = {
        let Ok(mut s) = DISCOVER.lock() else {
            return;
        };
        if s.loading || s.loading_more || !s.has_more || !s.query.trim().is_empty() {
            return;
        }
        s.loading_more = true;
        s.generation
    };
    publish_discover();
    fetch_discover(generation);
}

pub fn discover_browse_search(query: String) {
    {
        let Ok(mut s) = DISCOVER.lock() else {
            return;
        };
        s.query = query;
    }
    publish_discover();
}

pub fn discover_browse_set_view_mode(mode: String) {
    {
        let Ok(mut s) = DISCOVER.lock() else {
            return;
        };
        s.view_mode = if mode == "list" {
            "list".to_string()
        } else {
            "grid".to_string()
        };
    }
    publish_discover();
}

/// The shared "discover" genre selection changed while this page is on
/// screen: re-fetch from offset 0 (the Slint reaches the same place through
/// `GenreFilterActions` -> the browse controller's refetch;
/// `genre_filter_qt::after_selection_change` only knows how to reload Home).
pub fn discover_browse_genre_changed() {
    let generation = {
        let Ok(mut s) = DISCOVER.lock() else {
            return;
        };
        if s.endpoint.is_empty() {
            return;
        }
        s.generation = s.generation.wrapping_add(1);
        s.next_offset = 0;
        s.has_more = false;
        s.cards.clear();
        s.loading = true;
        s.loading_more = false;
        s.generation
    };
    publish_discover();
    fetch_discover(generation);
}

fn fetch_discover(generation: u64) {
    let runtime = crate::app();
    let (endpoint, offset) = {
        let Ok(s) = DISCOVER.lock() else {
            return;
        };
        (s.endpoint.clone(), s.next_offset)
    };
    crate::spawn(async move {
        let genre_ids = crate::genre_filter_qt::discover_genre_ids();
        match runtime
            .core()
            .get_discover_albums(&endpoint, genre_ids, offset, PAGE_SIZE)
            .await
        {
            Ok(data) => {
                let has_more = data.has_more;
                let fetched = data.items.len() as u32;
                let mut cards: Vec<HomeCard> = data
                    .items
                    .into_iter()
                    .map(crate::home_qt::map_album)
                    .collect();
                // The return is the MISSING list, and nobody wants it any more —
                // see the note at the publish below. This call is kept purely
                // for its disk-cache side effect.
                let _ = attach_card_art(&mut cards);
                {
                    let Ok(mut s) = DISCOVER.lock() else {
                        return;
                    };
                    if s.generation != generation {
                        return;
                    }
                    s.cards.extend(cards);
                    s.next_offset = offset.saturating_add(fetched);
                    s.has_more = has_more && fetched > 0;
                    s.loading = false;
                    s.loading_more = false;
                }
                publish_discover();
                // THE BATCH DOWNLOAD IS GONE, and its republish with it.
                // It fetched every missing cover of the page before showing
                // ANY of them, then re-attached the paths and republished the
                // whole document — so a View-all page had no artwork until it
                // had all of it, and the republish rebuilt every delegate on
                // arrival. The view asks for what is near its viewport now and
                // takes the answers one key at a time
                // (`QbzShell.sidebarArtworkWindow` -> `libraryArtworkReady`),
                // which is the shape Library > All already uses.
                //
                // `attach_card_art` above stays: it is a pure disk-cache
                // lookup, so anything already downloaded still renders on the
                // first frame with no request at all.
            }
            Err(e) => {
                log::warn!("[qbz-qt] discover browse fetch failed ({endpoint}): {e}");
                {
                    let Ok(mut s) = DISCOVER.lock() else {
                        return;
                    };
                    if s.generation != generation {
                        return;
                    }
                    s.loading = false;
                    s.loading_more = false;
                    s.has_more = false;
                }
                publish_discover();
            }
        }
    });
}

// ===========================================================================
//  PlaylistBrowse
// ===========================================================================

#[derive(Clone, serde::Serialize)]
struct TagRow {
    slug: String,
    name: String,
    selected: bool,
}

struct PlaylistBrowse {
    title: String,
    tags: Vec<TagRow>,
    /// Server-side category. Cleared ONLY on a fresh open from the rail
    /// (playlist_browse.rs:118-121 `reset_tag`).
    selected_tag: String,
    next_offset: u32,
    has_more: bool,
    cards: Vec<HomeCard>,
    query: String,
    view_mode: String,
    loading: bool,
    loading_more: bool,
    generation: u64,
}

impl PlaylistBrowse {
    const fn new() -> Self {
        Self {
            title: String::new(),
            tags: Vec::new(),
            selected_tag: String::new(),
            next_offset: 0,
            has_more: false,
            cards: Vec::new(),
            query: String::new(),
            view_mode: String::new(),
            loading: false,
            loading_more: false,
            generation: 0,
        }
    }
}

static PLAYLISTS: Mutex<PlaylistBrowse> = Mutex::new(PlaylistBrowse::new());

fn publish_playlists() {
    let (json, loading, loading_more) = {
        let Ok(s) = PLAYLISTS.lock() else {
            return;
        };
        let doc = format!(
            r#"{{"title":{},"tags":{},"selectedTag":{},"viewMode":{},"query":{},"hasMore":{},"total":{},"items":{}}}"#,
            json_str(&s.title),
            serde_json::to_string(&s.tags).unwrap_or_else(|_| "[]".to_string()),
            json_str(&s.selected_tag),
            json_str(&s.view_mode),
            json_str(&s.query),
            s.has_more,
            s.cards.len(),
            visible_json(&s.cards, &s.query),
        );
        (doc, s.loading, s.loading_more)
    };
    crate::home_bridge::ui(move |mut b| {
        b.as_mut()
            .set_playlist_browse_json(QString::from(json.as_str()));
        b.as_mut().set_playlist_browse_loading(loading);
        b.as_mut().set_playlist_browse_loading_more(loading_more);
    });
}

/// "View all" on the Qobuz Playlists rail. The search query clears on a
/// fresh navigation; the view mode AND the selected tag persist (the Slint
/// clears the tag only through `reset_tag`, which this entry point IS —
/// `main.rs:17519 on_open_playlist_browse` passes `reset_tag = true`).
pub fn open_playlist_browse() {
    if crate::offline_fwd::engine().status().is_offline() {
        return;
    }
    crate::nav_qt::record("playlistbrowse");
    let generation = {
        let Ok(mut s) = PLAYLISTS.lock() else {
            return;
        };
        let mode = if s.view_mode.is_empty() {
            "grid".to_string()
        } else {
            s.view_mode.clone()
        };
        s.generation = s.generation.wrapping_add(1);
        s.title = qbz_i18n::t("Qobuz Playlists");
        s.selected_tag.clear();
        for tag in s.tags.iter_mut() {
            tag.selected = false;
        }
        s.next_offset = 0;
        s.has_more = false;
        s.cards.clear();
        s.query.clear();
        s.view_mode = mode;
        s.loading = true;
        s.loading_more = false;
        s.generation
    };
    publish_playlists();
    fetch_tags_once();
    fetch_playlists(generation);
}

pub fn playlist_browse_load_more() {
    let generation = {
        let Ok(mut s) = PLAYLISTS.lock() else {
            return;
        };
        if s.loading || s.loading_more || !s.has_more || !s.query.trim().is_empty() {
            return;
        }
        s.loading_more = true;
        s.generation
    };
    publish_playlists();
    fetch_playlists(generation);
}

pub fn playlist_browse_search(query: String) {
    {
        let Ok(mut s) = PLAYLISTS.lock() else {
            return;
        };
        s.query = query;
    }
    publish_playlists();
}

pub fn playlist_browse_set_view_mode(mode: String) {
    {
        let Ok(mut s) = PLAYLISTS.lock() else {
            return;
        };
        s.view_mode = if mode == "list" {
            "list".to_string()
        } else {
            "grid".to_string()
        };
    }
    publish_playlists();
}

/// Category radio: SERVER-side, so the catalog is re-fetched from offset 0.
pub fn playlist_browse_select_tag(slug: String) {
    let generation = {
        let Ok(mut s) = PLAYLISTS.lock() else {
            return;
        };
        s.selected_tag = slug.clone();
        for tag in s.tags.iter_mut() {
            tag.selected = tag.slug == slug;
        }
        s.generation = s.generation.wrapping_add(1);
        s.next_offset = 0;
        s.has_more = false;
        s.cards.clear();
        s.loading = true;
        s.loading_more = false;
        s.generation
    };
    publish_playlists();
    fetch_playlists(generation);
}

/// Same shape as the discover page: the shared genre selection is
/// server-side here too.
pub fn playlist_browse_genre_changed() {
    let generation = {
        let Ok(mut s) = PLAYLISTS.lock() else {
            return;
        };
        s.generation = s.generation.wrapping_add(1);
        s.next_offset = 0;
        s.has_more = false;
        s.cards.clear();
        s.loading = true;
        s.loading_more = false;
        s.generation
    };
    publish_playlists();
    fetch_playlists(generation);
}

/// `/playlist/getTags` once per session — the tag bar is static per locale
/// and a re-fetch on every visit would flash the bar in and out.
fn fetch_tags_once() {
    {
        let Ok(s) = PLAYLISTS.lock() else {
            return;
        };
        if !s.tags.is_empty() {
            return;
        }
    }
    let runtime = crate::app();
    crate::spawn(async move {
        match runtime.core().get_playlist_tags().await {
            Ok(tags) => {
                {
                    let Ok(mut s) = PLAYLISTS.lock() else {
                        return;
                    };
                    let selected = s.selected_tag.clone();
                    s.tags = tags
                        .into_iter()
                        .map(|t| TagRow {
                            selected: t.slug == selected,
                            slug: t.slug,
                            name: t.name,
                        })
                        .collect();
                }
                publish_playlists();
            }
            Err(e) => log::warn!("[qbz-qt] playlist tags fetch failed: {e}"),
        }
    });
}

/// `owner   •   N tracks` (playlist_browse.rs:59-73 — THREE spaces each side
/// of the bullet; owner alone when the count is 0).
fn playlist_subtitle(owner: &str, tracks: u32) -> String {
    if tracks == 0 {
        return owner.to_string();
    }
    let count = qbz_i18n::tf(
        "{} track",
        "{} tracks",
        tracks as i64,
        &[tracks.to_string().as_str()],
    );
    format!("{owner}   •   {count}")
}

fn fetch_playlists(generation: u64) {
    let runtime = crate::app();
    let (tag, offset) = {
        let Ok(s) = PLAYLISTS.lock() else {
            return;
        };
        (
            if s.selected_tag.is_empty() {
                None
            } else {
                Some(s.selected_tag.clone())
            },
            s.next_offset,
        )
    };
    crate::spawn(async move {
        let genre_ids = crate::genre_filter_qt::discover_genre_ids();
        match runtime
            .core()
            .get_discover_playlists(tag, genre_ids, Some(PAGE_SIZE), Some(offset))
            .await
        {
            Ok(page) => {
                let has_more = page.has_more;
                let fetched = page.items.len() as u32;
                let mut cards: Vec<HomeCard> = page
                    .items
                    .into_iter()
                    .map(|p| {
                        let owner = if p.owner.name.is_empty() {
                            "Qobuz".to_string()
                        } else {
                            p.owner.name.clone()
                        };
                        let tracks = p.tracks_count;
                        let mut card = crate::home_qt::map_playlist(p);
                        card.subtitle = playlist_subtitle(&owner, tracks);
                        card
                    })
                    .collect();
                // The return is the MISSING list, and nobody wants it any more —
                // see the note at the publish below. This call is kept purely
                // for its disk-cache side effect.
                let _ = attach_card_art(&mut cards);
                {
                    let Ok(mut s) = PLAYLISTS.lock() else {
                        return;
                    };
                    if s.generation != generation {
                        return;
                    }
                    s.cards.extend(cards);
                    s.next_offset = offset.saturating_add(fetched);
                    s.has_more = has_more && fetched > 0;
                    s.loading = false;
                    s.loading_more = false;
                }
                publish_playlists();
                // THE BATCH DOWNLOAD IS GONE, and its republish with it.
                // It fetched every missing cover of the page before showing
                // ANY of them, then re-attached the paths and republished the
                // whole document — so a View-all page had no artwork until it
                // had all of it, and the republish rebuilt every delegate on
                // arrival. The view asks for what is near its viewport now and
                // takes the answers one key at a time
                // (`QbzShell.sidebarArtworkWindow` -> `libraryArtworkReady`),
                // which is the shape Library > All already uses.
                //
                // `attach_card_art` above stays: it is a pure disk-cache
                // lookup, so anything already downloaded still renders on the
                // first frame with no request at all.
            }
            Err(e) => {
                log::warn!("[qbz-qt] playlist browse fetch failed: {e}");
                {
                    let Ok(mut s) = PLAYLISTS.lock() else {
                        return;
                    };
                    if s.generation != generation {
                        return;
                    }
                    s.loading = false;
                    s.loading_more = false;
                    s.has_more = false;
                }
                publish_playlists();
            }
        }
    });
}

// ===========================================================================
//  Play history — Recently Played Albums / Most Played Albums
// ===========================================================================

struct PlayHistory {
    /// "recent" | "mostPlayed" — the view arms off this, so it travels on
    /// the document instead of being re-derived from `currentView` (a back
    /// navigation would race that).
    mode: String,
    cards: Vec<HomeCard>,
    search: String,
    loading: bool,
    generation: u64,
}

impl PlayHistory {
    const fn new() -> Self {
        Self {
            mode: String::new(),
            cards: Vec::new(),
            search: String::new(),
            loading: false,
            generation: 0,
        }
    }
}

static HISTORY: Mutex<PlayHistory> = Mutex::new(PlayHistory::new());

fn publish_history() {
    let (json, loading) = {
        let Ok(s) = HISTORY.lock() else {
            return;
        };
        let doc = format!(
            r#"{{"mode":{},"search":{},"total":{},"items":{}}}"#,
            json_str(&s.mode),
            json_str(&s.search),
            s.cards.len(),
            serde_json::to_string(&s.cards).unwrap_or_else(|_| "[]".to_string()),
        );
        (doc, s.loading)
    };
    crate::home_bridge::ui(move |mut b| {
        b.as_mut()
            .set_play_history_json(QString::from(json.as_str()));
        b.as_mut().set_play_history_loading(loading);
    });
}

/// NO offline gate on either history route: the Slint mounts both while
/// offline (AppShell.slint:456-470 — neither appears in `qobuz-view-blocked`
/// at :112-136) because both read a LOCAL store.
pub fn open_recent_albums() {
    crate::nav_qt::record("recentalbums");
    let generation = {
        let Ok(mut s) = HISTORY.lock() else {
            return;
        };
        s.generation = s.generation.wrapping_add(1);
        s.mode = "recent".to_string();
        s.cards.clear();
        s.search.clear();
        s.loading = true;
        s.generation
    };
    publish_history();
    load_history(generation);
}

pub fn open_most_played_albums() {
    crate::nav_qt::record("mostplayedalbums");
    let generation = {
        let Ok(mut s) = HISTORY.lock() else {
            return;
        };
        s.generation = s.generation.wrapping_add(1);
        s.mode = "mostPlayed".to_string();
        s.cards.clear();
        // The opener resets the box in the same hop that raises `loading`
        // (Slint main.rs:3860-3869).
        s.search.clear();
        s.loading = true;
        s.generation
    };
    publish_history();
    load_history(generation);
}

/// Most Played only: lowercase-contains over title/artist across the WHOLE
/// store (Slint main.rs:3877-3897 — the filter re-derives from
/// `MOST_PLAYED_ROWS`, it does not narrow the already-filtered model).
pub fn most_played_filter(query: String) {
    let generation = {
        let Ok(mut s) = HISTORY.lock() else {
            return;
        };
        if s.mode != "mostPlayed" {
            return;
        }
        s.search = query;
        s.generation
    };
    load_history(generation);
}

fn load_history(generation: u64) {
    crate::spawn(async move {
        let (mode, needle) = {
            let Ok(s) = HISTORY.lock() else {
                return;
            };
            (s.mode.clone(), s.search.trim().to_lowercase())
        };
        // Both stores are local SQLite / JSON reads; keep them off the Qt
        // thread anyway (the play-history table can hold thousands of rows).
        let filter = needle.clone();
        let mut cards: Vec<HomeCard> = tokio::task::spawn_blocking(move || {
            let needle = filter;
            if mode == "mostPlayed" {
                qbz_app::settings::album_play_history::all_albums()
                    .into_iter()
                    .filter(|r| {
                        needle.is_empty()
                            || r.title.to_lowercase().contains(&needle)
                            || r.artist.to_lowercase().contains(&needle)
                    })
                    .map(crate::home_qt::map_played_album)
                    .collect::<Vec<_>>()
            } else {
                crate::recently_qt::load_albums()
                    .into_iter()
                    .map(crate::home_qt::map_recent_album)
                    .collect::<Vec<_>>()
            }
        })
        .await
        .unwrap_or_default();

        let missing = attach_card_art(&mut cards);
        {
            let Ok(mut s) = HISTORY.lock() else {
                return;
            };
            // Generation guards the mode switch; the needle guards a
            // keystroke race — typing "abc" fires three overlapping tasks and
            // a slow "ab" landing last would repaint stale rows under a box
            // that reads "abc".
            if s.generation != generation || s.search.trim().to_lowercase() != needle {
                return;
            }
            s.cards = cards;
            s.loading = false;
        }
        publish_history();
        if !missing.is_empty() {
            crate::artwork_qt::download_missing(missing).await;
            let mut cards = {
                let Ok(mut s) = HISTORY.lock() else {
                    return;
                };
                if s.generation != generation {
                    return;
                }
                std::mem::take(&mut s.cards)
            };
            let _ = attach_card_art(&mut cards);
            {
                let Ok(mut s) = HISTORY.lock() else {
                    return;
                };
                if s.generation != generation {
                    return;
                }
                s.cards = cards;
            }
            publish_history();
        }
    });
}

// ===========================================================================
//  Live language switch
// ===========================================================================

/// Re-publish whichever browse page is on screen after a language change.
/// The Rust-built strings on these documents are the DiscoverBrowse title
/// (it arrives already translated from the rail), the playlist page title
/// and the playlist row subtitles (`tf()` plurals) — everything else is
/// `@tr` on the QML side and re-evaluates on `trRev`.
///
/// Called by `main.rs::apply_language`; `trRev` cannot retranslate strings
/// that already arrived inside a JSON document.
pub fn republish_for_language() {
    {
        if let Ok(mut s) = PLAYLISTS.lock() {
            if !s.title.is_empty() {
                s.title = qbz_i18n::t("Qobuz Playlists");
            }
        }
    }
    publish_discover();
    publish_playlists();
    publish_history();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(title: &str, artist: &str) -> HomeCard {
        HomeCard {
            title: title.to_string(),
            artist: artist.to_string(),
            ..HomeCard::default()
        }
    }

    #[test]
    fn search_matches_title_or_artist_case_insensitively() {
        let c = card("Kind of Blue", "Miles Davis");
        assert!(matches(&c, ""));
        assert!(matches(&c, "blue"));
        assert!(matches(&c, "miles"));
        assert!(!matches(&c, "coltrane"));
    }

    #[test]
    fn playlist_subtitle_shape() {
        // Owner alone when the playlist reports no tracks.
        assert_eq!(playlist_subtitle("Qobuz", 0), "Qobuz");
        // Three spaces each side of the bullet (playlist_browse.rs:59-73).
        let s = playlist_subtitle("Qobuz", 12);
        assert!(s.starts_with("Qobuz   •   "), "unexpected subtitle: {s}");
        assert!(s.contains("12"));
    }

    #[test]
    fn visible_json_filters() {
        let cards = vec![
            card("Blue Train", "Coltrane"),
            card("Giant Steps", "Coltrane"),
        ];
        assert!(visible_json(&cards, "blue").contains("Blue Train"));
        assert!(!visible_json(&cards, "blue").contains("Giant Steps"));
        assert!(visible_json(&cards, "").contains("Giant Steps"));
    }
}
