//! Dedicated artist discography page — the Qt half of
//! `crates/qbz/src/artist_releases.rs` (99 lines, the whole file) plus the four
//! `main.rs` handlers that drive it:
//!
//!  - `main.rs:15055-15087` `.on_open_releases`     -> `open`
//!  - `main.rs:3570-3607`   `navigate_artist_releases` (the fetch half)
//!  - `main.rs:15098-15135` `.on_load_more`         -> `load_more`
//!  - `main.rs:15143-15148` `.on_set_sort`          -> `set_sort`
//!  - `main.rs:15158-15176` `.on_retry`             -> `retry`
//!
//! The page is the full, paginated grid for ONE release bucket of ONE artist,
//! reached from the artist page's "See discography" link
//! (`artist/ReleaseGrid.slint:54-69`) and from the album page's "From the same
//! artist" View-all (`album/AlbumPageView.slint:1123-1136`, which pins the
//! bucket to `"album"`). Two doors, one page — hence `open` takes the artist
//! NAME as a parameter instead of reading it off the artist document: the album
//! page supplies its own (`main.rs:11844-11848`).
//!
//! # Why this file does NOT call `artist_qt::load_release_page`
//!
//! That function exists for the artist page's per-section "Load more" and it
//! ends in `merge_release_page`, which folds every fetched page into the
//! STASHED ARTIST DOCUMENT (`artist_qt::ARTIST_DOC`). Paging this page ten deep
//! would silently grow the artist page's own "Albums" grid to 200 cards behind
//! the user's back. The reference keeps `ArtistState.release-sections` and
//! `ArtistReleasesState.albums` strictly disjoint (two states, one endpoint),
//! so this file issues the identical core call itself:
//!
//! ```text
//! get_releases_grid(id, release_type, RELEASE_PAGE_SIZE, offset, Some(order))
//! ```
//!
//! Same 20-row page size. Default/date/title use release_date pages; Popularity
//! uses relevant pages. Changing between them restarts pagination.
//!
//! # Why the rows are `home_qt::HomeCard`, not `album_qt::AlbumCardData`
//!
//! The view mounts the shared `views/AlbumCollection.qml`, whose delegates read
//! `modelData.artPath` (`AlbumCollection.qml:391`) — a Rust-resolved cache path.
//! `AlbumCardData` carries `artUrl` and NO `artPath`, which is why the artist
//! page resolves its covers in QML through `QbzShell.sidebarArtworkWindow` ->
//! `QbzLibrary.libraryArtworkReady` -> its own `coverMap`. Publishing
//! `AlbumCardData` into `AlbumCollection` compiles, mounts, and draws every
//! cover BLANK. So the fetch runs the `label_qt::fetch_releases` two-pass
//! (`label_qt.rs:800-847`): `home_qt::attach_card_art` for what is already on
//! disk, publish, `artwork_qt::download_missing`, re-attach, publish again.
//!
//! # The subtitle slot carries the YEAR
//!
//! `qbz/src/artist.rs:670-698` `card_to_item`: "the card subtitle slot should
//! show the release year — the artist is redundant since we're already on their
//! page". It routes `year` through the card's `artist` field and blanks
//! `artist_id` so the line is inert, while `plain_year` keeps the real value
//! for the sort. `HomeCard` has no `plain_year`, so here `artist` AND `year`
//! both carry the year and `sort_cards` reads `year` — never `artist`, which
//! only happens to work while every release has one.

use std::collections::HashSet;
use std::sync::Mutex;

use cxx_qt_lib::QString;
use qbz_models::PageArtistRelease;
use serde::Serialize;

use crate::home_qt::HomeCard;

/// Page state. ONE bucket of ONE artist at a time — navigating to another
/// bucket resets it wholesale, exactly as `artist_releases::reset` does.
#[derive(Default)]
struct State {
    /// Bumped by every `open` / `retry`. A page that lands for a previous
    /// bucket (or a previous artist) is DROPPED rather than painted — the
    /// `label_qt.rs:697` guard. It matters more here than there: the nav stack
    /// stores no payload (`nav_qt.rs:9-23`), so back/forward re-mounts this
    /// view against whatever document is still on the bridge, and a late page
    /// painting into the wrong bucket would be indistinguishable from data.
    generation: u64,
    artist_id: String,
    name: String,
    release_type: String,
    /// The translated bucket title (`artist_qt::release_type_title`). Built in
    /// RUST, so it cannot re-translate through `trRev` — see `republish`.
    title: String,
    cards: Vec<HomeCard>,
    /// Rows the SERVER has handed over, which is the offset of the next page.
    ///
    /// Deliberately NOT `cards.len()` (the reference's `loaded_count`,
    /// `artist_releases.rs:94-99`): blacklist filtering and id-dedup both make
    /// the rendered count smaller than the fetched one, and feeding the smaller
    /// number back as an offset re-requests rows already seen and skips the
    /// tail. Invisible with an empty blacklist, which is why the reference gets
    /// away with it.
    offset: u32,
    has_more: bool,
    loading: bool,
    load_more_loading: bool,
    load_error: bool,
    /// `default` | `newest` | `oldest` | `title-asc` | `title-desc` — the same
    /// five wire strings the artist page's per-section picker persists, and the
    /// same store (`artist_prefs`). Seeded on open from the persisted value
    /// (`artist_releases.rs:25`), so a bucket the user sorted A–Z opens A–Z.
    sort_by: String,
}

static RELEASES: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = RELEASES.lock().unwrap();
    f(guard.get_or_insert_with(State::default))
}

/// The ONE document this page paints from (`ui/state.slint:3996-4019`
/// `ArtistReleasesState`, camelCased for QML).
///
/// `loading` lives INSIDE the document rather than on a second bridge property.
/// `label_qt` publishes `labelReleasesLoading` separately only because it
/// predates the single-document convention; this page has no cross-view reader,
/// so one property is one fewer bridge member and one fewer thing to keep in
/// sync. QML re-parses on every change, so a fresh string is a fresh object —
/// the "Rebind requires a NEW object reference" rule
/// (`cards/PlaylistCollage.qml`) is satisfied by construction.
#[derive(Serialize)]
struct ArtistReleasesDoc<'a> {
    id: &'a str,
    name: &'a str,
    #[serde(rename = "releaseType")]
    release_type: &'a str,
    title: &'a str,
    albums: &'a [HomeCard],
    #[serde(rename = "hasMore")]
    has_more: bool,
    loading: bool,
    #[serde(rename = "loadMoreLoading")]
    load_more_loading: bool,
    #[serde(rename = "loadError")]
    load_error: bool,
    #[serde(rename = "sortBy")]
    sort_by: &'a str,
}

fn publish() {
    let json = with_state(|s| {
        let doc = ArtistReleasesDoc {
            id: &s.artist_id,
            name: &s.name,
            release_type: &s.release_type,
            title: &s.title,
            albums: &s.cards,
            has_more: s.has_more,
            loading: s.loading,
            load_more_loading: s.load_more_loading,
            load_error: s.load_error,
            sort_by: &s.sort_by,
        };
        serde_json::to_string(&doc).unwrap_or_else(|_| "{}".to_string())
    });
    crate::artist_bridge::ui(move |mut b| {
        b.as_mut()
            .set_artist_releases_json(QString::from(json.as_str()));
    });
}

// ---------------------------------------------------------------------------
//  Mapping
// ---------------------------------------------------------------------------

/// One server release -> one `AlbumCollection` row, or `None` when the album is
/// blacklisted.
///
/// The blacklist test runs on the release's REAL artist id — i.e. BEFORE the
/// subtitle transposition blanks it (`qbz/src/artist.rs:167` and
/// `artist_releases.rs:50-52` both filter; `artist_qt::load_release_page` does
/// NOT, which is a separate gap on the artist page's own Load-more path).
/// Getting the order wrong here would leave the album axis working and silently
/// disable the artist axis.
fn map_card(release: &PageArtistRelease) -> Option<HomeCard> {
    // Reuse the artist page's mapper for every field value (pin badge, heart,
    // quality tier + detail, genre, the 4-digit year, the best cover url).
    let card = crate::artist_qt::map_release(release);
    if crate::artist_blacklist::card_blacklisted(&card.id, &card.artist_id) {
        return None;
    }
    Some(HomeCard {
        id: card.id,
        title: card.title,
        // The YEAR, not the artist — see the file header.
        artist: card.year.clone(),
        artist_id: String::new(),
        genre: card.genre,
        release_sort_key: card.release_sort_key,
        year: card.year,
        quality_tier: card.quality_tier,
        quality_detail: card.quality_detail,
        art_url: card.art_url,
        is_pinned: card.is_pinned,
        is_favorite: card.is_favorite,
        // ribbon / ribbon_kind / rank / plays / category / playlist_* stay
        // empty: this grid is albums only and mounts no ribbon.
        ..Default::default()
    })
}

/// `album_map::sort_album_items` (`qbz/src/album_map.rs:246-264`) — the twin of
/// `label_qt::sort_cards`, minus the artist arms (this page's picker has five
/// options and stops at title-desc, and `artist` holds the year here anyway).
///
/// `release_sort_key` carries the original date independently of the localized
/// display label in `year`.
///
/// Default and Popularity restore the server ordinal, including after local
/// sorts and appended pages. The fetch selects the matching server order.
fn sort_cards(items: &mut [HomeCard], sort: &str) {
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
        _ => {}
    }
}

// ---------------------------------------------------------------------------
//  Entry points (QbzArtist invokables)
// ---------------------------------------------------------------------------

/// `artist_releases::reset` (`artist_releases.rs:14-26`) — the header is seeded
/// and the grid enters its loading state BEFORE anything is fetched, which is
/// also the order `navigate_artist_releases` uses (`main.rs:3583-3591`: the view
/// flips first, so the page mounts already spinning).
fn reset(artist_id: &str, name: &str, release_type: &str) -> u64 {
    with_state(|s| {
        s.generation = s.generation.wrapping_add(1);
        s.artist_id = artist_id.to_string();
        s.name = name.to_string();
        s.release_type = release_type.to_string();
        s.title = crate::artist_qt::release_type_title(release_type);
        s.cards.clear();
        s.offset = 0;
        s.has_more = false;
        s.loading = true;
        s.load_more_loading = false;
        s.load_error = false;
        s.sort_by = crate::artist_prefs::get_sort(release_type);
        s.generation
    })
}

/// "See discography" / the album page's "From the same artist" View all.
///
/// The two guards are the reference's own: the whole view is mounted under
/// `!OfflineState.offline` (`shell/AppShell.slint:503-511`), and
/// `.on_open_releases` returns silently on an empty artist id
/// (`main.rs:15065`). Neither logs — an empty id means the caller has no artist
/// on screen, not that something failed.
pub fn open(artist_id: String, artist_name: String, release_type: String) {
    if crate::offline_fwd::engine().status().is_offline() {
        return;
    }
    if artist_id.is_empty() {
        return;
    }
    let generation = reset(&artist_id, &artist_name, &release_type);
    // The route is a TWO-FILE contract (nav_qt.rs:9-16): this records the id,
    // AppShell.qml's Loader ternary mounts it. `QbzShell.navigateTo` is NOT
    // usable — it carries no payload and clears LAST_DETAIL (main.rs:1607).
    crate::nav_qt::record_with(
        "artistreleases",
        serde_json::json!({
            "artistId": &artist_id,
            "artistName": &artist_name,
            "releaseType": &release_type
        }),
    );
    publish();
    fetch(generation, artist_id, release_type, 0, true);
}

/// Infinite-scroll tail (`main.rs:15098-15135`). Bails when a page is already
/// in flight or the server said there is nothing left; the first page's
/// `loading` counts as in-flight too (the .slint's own guard,
/// `ArtistReleasesView.slint:123-128`, tests all three).
pub fn load_more() {
    let Some((generation, artist_id, release_type, offset)) = with_state(|s| {
        if s.loading || s.load_more_loading || !s.has_more {
            return None;
        }
        s.load_more_loading = true;
        Some((
            s.generation,
            s.artist_id.clone(),
            s.release_type.clone(),
            s.offset,
        ))
    }) else {
        return;
    };
    publish();
    fetch(generation, artist_id, release_type, offset, false);
}

/// Popularity is the server's relevant order; reload from page zero when
/// switching its pagination order. Other sorts keep the loaded set.
pub fn set_sort(sort: String) {
    let release_type = with_state(|s| s.release_type.clone());
    crate::artist_prefs::set_sort(&release_type, &sort);
    let reload = with_state(|s| {
        let changed = crate::artist_qt::release_server_sort(&s.sort_by)
            != crate::artist_qt::release_server_sort(&sort);
        s.sort_by = sort;
        if changed {
            s.generation = s.generation.wrapping_add(1);
            s.cards.clear();
            s.offset = 0;
            s.has_more = false;
            s.loading = true;
            s.load_more_loading = false;
            s.load_error = false;
            Some((s.generation, s.artist_id.clone(), s.release_type.clone()))
        } else {
            sort_cards(&mut s.cards, &s.sort_by);
            None
        }
    });
    publish();
    if let Some((generation, artist_id, release_type)) = reload {
        fetch(generation, artist_id, release_type, 0, true);
    }
}

/// The error state's Retry button (`main.rs:15158-15176`) — a full re-run of
/// the open body from the state's OWN id/name/release_type, and deliberately
/// WITHOUT `nav_qt::record`: the user is already standing on this entry, and
/// pushing a second one would make Back a no-op.
pub fn retry() {
    let Some((artist_id, name, release_type)) = with_state(|s| {
        if s.artist_id.is_empty() {
            return None;
        }
        Some((s.artist_id.clone(), s.name.clone(), s.release_type.clone()))
    }) else {
        return;
    };
    if crate::offline_fwd::engine().status().is_offline() {
        return;
    }
    let generation = reset(&artist_id, &name, &release_type);
    publish();
    fetch(generation, artist_id, release_type, 0, true);
}

/// Live language switch. The bucket title is a Rust-translated string sitting
/// INSIDE a JSON document, so `QbzSession.trRev` cannot re-translate it — the
/// same reason `myqbz_qt::republish_all` is called unconditionally from
/// `apply_language`. A no-op on an empty document (nothing has been opened).
///
/// Wired from `main.rs::apply_language` alongside Browse and Label republish.
pub fn republish() {
    let has_page = with_state(|s| {
        if s.artist_id.is_empty() {
            return false;
        }
        s.title = crate::artist_qt::release_type_title(&s.release_type);
        true
    });
    if has_page {
        publish();
    }
}

// ---------------------------------------------------------------------------
//  Fetch
// ---------------------------------------------------------------------------

/// One page. `first` distinguishes the two error arms the reference keeps
/// apart: the FIRST page failing raises the retryable error state
/// (`main.rs:3599-3605`), a later page failing only clears
/// `load_more_loading` and leaves what is already on screen alone
/// (`main.rs:15129`).
fn fetch(generation: u64, artist_id: String, release_type: String, offset: u32, first: bool) {
    let order = with_state(|s| crate::artist_qt::release_server_sort(&s.sort_by));
    let runtime = crate::app();
    crate::spawn(async move {
        let Ok(id) = artist_id.parse::<u64>() else {
            log::warn!("[qbz-qt] artist releases: invalid artist id {artist_id}");
            fail(generation, first);
            return;
        };
        let resp = match runtime
            .core()
            .get_releases_grid(
                id,
                &release_type,
                crate::artist_qt::RELEASE_PAGE_SIZE,
                offset,
                Some(order),
            )
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                log::warn!(
                    "[qbz-qt] artist releases page failed ({artist_id}/{release_type}): {e}"
                );
                fail(generation, first);
                return;
            }
        };

        let returned = resp.items.len() as u32;
        let server_has_more = resp.has_more;
        let mut cards: Vec<HomeCard> = resp
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, release)| {
                let mut card = map_card(release)?;
                card.default_order = offset.saturating_add(index as u32);
                Some(card)
            })
            .collect();
        let missing = crate::home_qt::attach_card_art(&mut cards);

        {
            let mut guard = RELEASES.lock().unwrap();
            let Some(s) = guard.as_mut() else {
                return;
            };
            if s.generation != generation {
                return;
            }
            // Dedupe by id against what is already loaded — Qobuz can repeat a
            // row across pages, and the reference dedupes on the same axis
            // (`artist_releases.rs:44-57`, its `seen` set).
            let seen: HashSet<String> = s.cards.iter().map(|c| c.id.clone()).collect();
            s.cards
                .extend(cards.into_iter().filter(|c| !seen.contains(&c.id)));
            s.offset = offset.saturating_add(returned);
            // `&& returned > 0` on top of the server flag: with the raw flag a
            // bucket that answers "has_more" with an empty page pins
            // `load_more` in a loop against an offset that never advances
            // (label_qt.rs:820 guards the same way).
            s.has_more = server_has_more && returned > 0;
            s.loading = false;
            s.load_more_loading = false;
            s.load_error = false;
            // Re-sort the FULL set, not just the arrival: under a custom sort a
            // page appended at the tail is simply in the wrong place
            // (`artist_releases.rs:62` sorts everything after the append).
            let sort = s.sort_by.clone();
            sort_cards(&mut s.cards, &sort);
        }
        publish();

        if missing.is_empty() {
            return;
        }
        // Second pass — covers that were not on disk yet. Same shape as
        // `label_qt::fetch_releases`: download, re-attach over the WHOLE set
        // (the page just landed sorted into the middle of it), republish.
        crate::artwork_qt::download_missing(missing).await;
        let mut all = {
            let mut guard = RELEASES.lock().unwrap();
            let Some(s) = guard.as_mut() else {
                return;
            };
            if s.generation != generation {
                return;
            }
            std::mem::take(&mut s.cards)
        };
        let _ = crate::home_qt::attach_card_art(&mut all);
        {
            let mut guard = RELEASES.lock().unwrap();
            let Some(s) = guard.as_mut() else {
                return;
            };
            if s.generation != generation {
                // The take above already emptied the state for a page the user
                // has navigated away from; `reset` has since refilled it, so
                // dropping `all` on the floor is correct.
                return;
            }
            s.cards = all;
        }
        publish();
    });
}

/// The two error arms (see `fetch`). Generation-guarded like every other write.
fn fail(generation: u64, first: bool) {
    let changed = with_state(|s| {
        if s.generation != generation {
            return false;
        }
        if first {
            s.loading = false;
            s.load_error = true;
            s.has_more = false;
        } else {
            s.load_more_loading = false;
        }
        true
    });
    if changed {
        publish();
    }
}

#[cfg(test)]
mod release_order_tests {
    use super::*;

    #[test]
    fn default_and_popularity_preserve_server_rank_after_local_sorts_and_pages() {
        let mut cards = vec![
            HomeCard {
                id: "beast".into(),
                default_order: 0,
                release_sort_key: 19820322,
                ..Default::default()
            },
            HomeCard {
                id: "mind".into(),
                default_order: 1,
                release_sort_key: 19830501,
                ..Default::default()
            },
            HomeCard {
                id: "senjutsu".into(),
                default_order: 2,
                release_sort_key: 20210903,
                ..Default::default()
            },
        ];
        sort_cards(&mut cards, "newest");
        assert_eq!(cards[0].id, "senjutsu");
        sort_cards(&mut cards, "relevant");
        assert_eq!(
            cards.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["beast", "mind", "senjutsu"]
        );
        cards.push(HomeCard {
            id: "page2".into(),
            default_order: 20,
            release_sort_key: 19800401,
            ..Default::default()
        });
        sort_cards(&mut cards, "oldest");
        assert_eq!(cards[0].id, "page2");
        sort_cards(&mut cards, "default");
        assert_eq!(cards.last().unwrap().id, "page2");
    }
}
