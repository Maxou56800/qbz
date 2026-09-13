//! Blacklist Manager controller — Slint-free port of
//! `crates/qbz/src/blacklist_manager.rs`.
//!
//! Reads the per-user blacklist through `crate::artist_blacklist` (the fail-open
//! singleton every other surface mutates too), applies search-as-you-type
//! filtering controller-side, and publishes ONE JSON document
//! (`QbzBlacklist.blacklistJson`) carrying all three axes. A third
//! "Recommendations" tab (`activeTab == 2`) lists the reco-SCOPED "Not
//! interested" dismissals from `crate::reco_dismiss_qt` — NOT the blacklist —
//! with a per-row undo.
//!
//! There is no change-notify on the blacklist store (the fav_cache pattern), so
//! the manager reloads on every open (`BlacklistManagerView.qml` calls
//! `reload()` in `Component.onCompleted`, because `nav_qt::back()` runs no
//! per-view load) and its own mutations re-push the filtered list in place.
//!
//! ## Two deltas vs the Slint controller, both forced by the transport
//!
//! 1. **`set_tab` republishes.** In Slint the active tab is its own global
//!    property (`BlacklistState.active-tab`) so `set_tab` only writes it. Here
//!    `activeTab` lives INSIDE the document, so a non-publishing `setTab` would
//!    be a dead button.
//! 2. **Every mutation also calls `crate::publish_settings()`.** The Settings
//!    row's counters live in a DIFFERENT document (`QbzBridge.settingsJson`) and
//!    `nav_qt::back()` re-runs no per-view load, so Manage -> unblock -> Back
//!    would otherwise show pre-mutation counters. Slint gets this free from the
//!    shared `BlacklistState` global plus its `seed_blacklist_status` on
//!    entering Settings.
//!
//! ## Toasts: wired, all 17
//!
//! An earlier draft dropped them for want of a toast host; `crate::toast_qt`
//! exists now (`toast_qt.rs:96-110`, the `in_app_toasts` gate applied Rust-side,
//! no window handle), so every reference toast is reproduced on the SAME
//! condition with the SAME message. Thirteen come from
//! `blacklist_manager.rs:220-351`; the other four are the `main.rs` menu
//! handlers whose Qt equivalents live in THIS file — `artist_toggle`
//! (`main.rs:12787-12817`), `album_toggle` (`main.rs:12846-12866`),
//! `dismiss_artist` (`main.rs:12986-13037`) — plus `block_album`, which the
//! reference routes through `blacklist_manager::block_album:287-292`.
//!
//! Two of them are `qbz_i18n::tf` plurals (the clear-all counts): the count is
//! formatted in RUST precisely because QML has no plural entry point, so
//! neither string ever crosses the bridge as a template.
//!
//! One deliberate wart is carried over verbatim: the artist visibility pair
//! (`"{name} is now visible"` / `"{name} is now hidden"`) and
//! `"Failed to update artist visibility"` are **not** translated in the
//! reference either — `main.rs:12799-12816` builds them with `format!` and a
//! bare `&str`, and the three msgids exist in none of the eight catalogs.
//! Reproduced as-is rather than silently diverging; translating them is a
//! both-sides change, not a port fix.
//!
//! ## Covers
//!
//! `albums[].artPath` is filled from the SYNCHRONOUS disk cache only
//! (`artwork_qt::cached_path`); nothing is downloaded on publish. The view
//! dispatches the visible window through [`artwork_window`] (debounced, on
//! scroll and on model change) and arrivals land on the existing
//! `QbzLibrary.libraryArtworkReady(key = cover url, path)` — the port's
//! artwork-is-never-in-the-document rule.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

use cxx_qt_lib::QString;
use serde::Serialize;

// ---------------------------------------------------------------------------
// The document (spec 03 §5). Per-FIELD serde renames — the crate's convention.
// ---------------------------------------------------------------------------

#[derive(Clone, Default, Serialize)]
struct ArtistRowDoc {
    /// String-carried u64 (no 2^31 truncation, and QML has no 64-bit int).
    id: String,
    name: String,
    #[serde(rename = "addedAt")]
    added_at: i64,
    notes: String,
    #[serde(rename = "hasNotes")]
    has_notes: bool,
}

#[derive(Clone, Default, Serialize)]
struct AlbumRowDoc {
    id: String,
    title: String,
    artist: String,
    #[serde(rename = "coverUrl")]
    cover_url: String,
    /// `file://` path from the disk cache, `""` until it resolves (the row
    /// shows the blind-eye fallback underneath either way).
    #[serde(rename = "artPath")]
    art_path: String,
    #[serde(rename = "addedAt")]
    added_at: i64,
    /// Carried for shape parity with the artist row; the Slint `AlbumRow`
    /// renders `added-display` in that slot and never the notes.
    notes: String,
    #[serde(rename = "hasNotes")]
    has_notes: bool,
}

#[derive(Clone, Default, Serialize)]
struct DismissedRowDoc {
    id: String,
    name: String,
}

#[derive(Clone, Default, Serialize)]
struct BlacklistDoc {
    #[serde(rename = "activeTab")]
    active_tab: i32,
    /// Shared by BOTH axes (artists + albums); the dismissals ignore it.
    enabled: bool,
    /// Echoed back so the field and the "No results for …" message agree.
    #[serde(rename = "searchQuery")]
    search_query: String,

    /// FULL length, never the filtered one — this is what separates "empty"
    /// from "no results".
    count: i32,
    artists: Vec<ArtistRowDoc>,

    #[serde(rename = "albumCount")]
    album_count: i32,
    albums: Vec<AlbumRowDoc>,

    #[serde(rename = "dismissedCount")]
    dismissed_count: i32,
    dismissed: Vec<DismissedRowDoc>,
}

// ---------------------------------------------------------------------------
// Controller state (Rust-side source of truth, like the reference)
// ---------------------------------------------------------------------------

/// The live search query. The view echoes it back from the document; this is
/// what the builders read, so a toggle/remove/clear re-push keeps the current
/// filter applied. Persists across navigation — the reference never clears it.
static QUERY: Mutex<String> = Mutex::new(String::new());

/// Active tab: 0 = Artists, 1 = Albums, 2 = Recommendations. Persists across
/// navigation, exactly like Slint's `BlacklistState.active-tab`.
static TAB: AtomicI32 = AtomicI32::new(0);

fn current_query() -> String {
    QUERY.lock().map(|q| q.clone()).unwrap_or_default()
}

fn set_query_value(q: String) {
    if let Ok(mut guard) = QUERY.lock() {
        *guard = q;
    }
}

// ---------------------------------------------------------------------------
// Builders — filtering is entirely Rust-side; QML never filters
// ---------------------------------------------------------------------------

/// Artists axis: substring on `artist_name` ONLY (notes are NOT searched),
/// preserving the store's `COLLATE NOCASE` order. Returns the filtered rows and
/// the FULL count.
fn build_artists() -> (Vec<ArtistRowDoc>, i32) {
    let all = crate::artist_blacklist::get_all();
    let count = all.len() as i32;
    let needle = current_query().trim().to_lowercase();

    let rows: Vec<ArtistRowDoc> = all
        .into_iter()
        .filter(|a| needle.is_empty() || a.artist_name.to_lowercase().contains(&needle))
        .map(|a| {
            let notes = a.notes.unwrap_or_default();
            ArtistRowDoc {
                id: a.artist_id.to_string(),
                name: a.artist_name,
                added_at: a.added_at,
                has_notes: !notes.is_empty(),
                notes,
            }
        })
        .collect();

    (rows, count)
}

/// Albums axis: substring on `album_title` OR `artist_name`, preserving the
/// store's `COLLATE NOCASE` order. Returns the filtered rows, the FULL count,
/// and the cover urls that are NOT on disk yet (the view dispatches those it can
/// actually see through [`artwork_window`]).
fn build_albums() -> (Vec<AlbumRowDoc>, i32, Vec<String>) {
    let all = crate::artist_blacklist::get_all_albums();
    let count = all.len() as i32;
    let needle = current_query().trim().to_lowercase();

    let mut missing: Vec<String> = Vec::new();
    let rows: Vec<AlbumRowDoc> = all
        .into_iter()
        .filter(|a| {
            needle.is_empty()
                || a.album_title.to_lowercase().contains(&needle)
                || a.artist_name.to_lowercase().contains(&needle)
        })
        .map(|a| {
            let notes = a.notes.unwrap_or_default();
            let art_path = if a.cover_url.is_empty() {
                String::new()
            } else {
                crate::artwork_qt::cached_path(&a.cover_url)
            };
            if art_path.is_empty() && !a.cover_url.is_empty() {
                missing.push(a.cover_url.clone());
            }
            AlbumRowDoc {
                id: a.album_id,
                title: a.album_title,
                artist: a.artist_name,
                cover_url: a.cover_url,
                art_path,
                added_at: a.added_at,
                has_notes: !notes.is_empty(),
                notes,
            }
        })
        .collect();

    (rows, count, missing)
}

/// Reco-dismissals axis: substring on `name`, in the store's FILE order (never
/// sorted). Returns the filtered rows and the FULL count.
fn build_dismissed() -> (Vec<DismissedRowDoc>, i32) {
    let all = crate::reco_dismiss_qt::list();
    let count = all.len() as i32;
    let needle = current_query().trim().to_lowercase();

    let rows: Vec<DismissedRowDoc> = all
        .into_iter()
        .filter(|a| needle.is_empty() || a.name.to_lowercase().contains(&needle))
        .map(|a| DismissedRowDoc {
            id: a.artist_id.to_string(),
            name: a.name,
        })
        .collect();

    (rows, count)
}

// ---------------------------------------------------------------------------
// Publish / load
// ---------------------------------------------------------------------------

/// Rebuild all three axes and push the ONE document. Synchronous: the wrapper
/// reads are an in-memory set plus a single SQLite query, exactly as in the
/// reference (there is no worker hop).
pub fn publish() {
    let (artists, count) = build_artists();
    let (albums, album_count, missing) = build_albums();
    let (dismissed, dismissed_count) = build_dismissed();
    if !missing.is_empty() {
        // Not downloaded here on purpose: the view window-dispatches through
        // `artwork_window`, so a 200-album blacklist never fetches 200 covers
        // to paint 8 rows.
        log::debug!(
            "[qbz-qt] blacklist: {} album covers not on disk yet",
            missing.len()
        );
    }
    let doc = BlacklistDoc {
        active_tab: TAB.load(Ordering::Relaxed),
        enabled: crate::artist_blacklist::is_enabled(),
        search_query: current_query(),
        count,
        artists,
        album_count,
        albums,
        dismissed_count,
        dismissed,
    };
    let json = serde_json::to_string(&doc).unwrap_or_else(|_| "{}".to_string());
    crate::blacklist_bridge::ui(move |mut b| {
        b.as_mut().set_blacklist_json(QString::from(json.as_str()));
    });
}

/// Load (or refresh) the manager: raise loading, read the store, push, clear
/// loading. The three hops are queued onto the Qt thread in order, so the view
/// never sees a published document while `blacklistLoading` is still true.
pub fn load() {
    crate::blacklist_bridge::ui(|mut b| b.as_mut().set_blacklist_loading(true));
    publish();
    crate::blacklist_bridge::ui(|mut b| b.as_mut().set_blacklist_loading(false));
}

/// Settings > Blacklist "Manage": push the route and load. `nav_qt::record`
/// republishes `canBack` / `canForward` / `currentView` in one hop, which is
/// what Slint's `open()` needs `update_nav_flags` for.
pub fn open_manager() {
    crate::nav_qt::record("blacklist");
    load();
}

/// Counters for the Settings row (`settings_qt::devtools`): enabled flag +
/// artist count + album count, straight off the per-user singleton. Replaces
/// the throwaway `BlacklistService` that was opened per settings publish. The
/// counts deliberately ignore the enabled flag.
pub fn counts() -> (bool, i32, i32) {
    (
        crate::artist_blacklist::is_enabled(),
        crate::artist_blacklist::count() as i32,
        crate::artist_blacklist::album_count() as i32,
    )
}

/// Cross-surface settle signal: `key` is the composite `"{kind}:{id}"` and
/// `value` is the SETTLED state (flipped on success, unchanged on failure) —
/// the same two-arg shape `QbzLibrary.pinChanged` / `libraryFavoriteChanged`
/// use, so a consumer's `Connections` block reads identically.
fn emit_changed(kind: &str, id: &str, value: bool) {
    let key = format!("{kind}:{id}");
    crate::blacklist_bridge::ui(move |mut b| {
        b.as_mut()
            .blacklist_changed(QString::from(key.as_str()), value);
    });
}

/// Republish the document AND the settings document. Every mutation ends here —
/// see the module docs for why the second one is not cosmetic.
fn republish_all() {
    publish();
    crate::publish_settings();
    // Discover keeps the raw index candidates in memory, so a blacklist
    // mutation can remove OR restore Home rows immediately without another
    // network fetch. This also refreshes the targeted recent-history rails.
    crate::home_qt::blacklist_changed();
    // Drop the 30-day Artist Scene cache (contract B3, requirement 3).
    //
    // THIS IS A CORRECTNESS CALL, NOT HOUSEKEEPING. Scene discovery caches a
    // validated candidate list, and a cache hit short-circuits the whole
    // validation loop — so a post-filter on the way out can hide a
    // newly-blocked artist, but it can NEVER undo a substitution. Concretely:
    // while artist X was blacklisted, validation resolved that MusicBrainz
    // candidate to a DIFFERENT same-name Qobuz artist and cached that. Un-block
    // X and the post-filter has nothing to restore — the row simply is not X
    // any more, and stays wrong for the full TTL. Only dropping the entry
    // re-runs validation.
    //
    // Placed in this funnel rather than at the four artist-axis mutators
    // because every mutation already routes through here and a fifth one added
    // later would silently miss the invalidation. The ALBUM-axis mutators trip
    // it too, which is over-invalidation — a scene lists artists, not albums.
    // That is an accepted cost: the penalty is one re-fetch, and the
    // alternative is a rule someone has to remember.
    crate::app().core().invalidate_scene_cache();
}

// ---------------------------------------------------------------------------
// Actions — search / tab / enable
// ---------------------------------------------------------------------------

/// Search-as-you-type (no debounce, matching the reference): store the query
/// and re-push. `count` stays the FULL length so the empty-vs-no-results split
/// stays correct.
pub fn search_changed(query: String) {
    set_query_value(query);
    publish();
}

/// Switch the active tab (0 Artists, 1 Albums, 2 Recommendations). Republishes
/// because `activeTab` lives inside the document.
pub fn set_tab(tab: i32) {
    TAB.store(tab.clamp(0, 2), Ordering::Relaxed);
    publish();
}

/// Toggle the global enable flag (the ONLY enable affordance in the feature —
/// the Settings row deliberately has none). The counts are unaffected by it.
pub fn toggle_enabled() {
    let new_state = !crate::artist_blacklist::is_enabled();
    match crate::artist_blacklist::set_enabled(new_state) {
        Ok(()) => {
            republish_all();
            // INFO, not success (`blacklist_manager.rs:220`).
            crate::toast_qt::info(if new_state {
                qbz_i18n::t("Blacklist enabled")
            } else {
                qbz_i18n::t("Blacklist disabled")
            });
        }
        Err(e) => {
            log::error!("[qbz-qt] blacklist toggle-enabled failed: {e}");
            crate::toast_qt::error(qbz_i18n::t("Failed to toggle blacklist"));
        }
    }
}

// ---------------------------------------------------------------------------
// Actions — artist axis
// ---------------------------------------------------------------------------

/// Manager row `x`: un-blacklist one artist (optimistic — the re-push drops the
/// row immediately). A non-numeric id is a silent no-op.
pub fn remove_artist(artist_id: String) {
    let Ok(id) = artist_id.parse::<u64>() else {
        log::debug!("[qbz-qt] blacklist remove skipped for non-numeric id '{artist_id}'");
        return;
    };
    // Name captured BEFORE the delete, for the toast — the row is gone after
    // (`blacklist_manager.rs:232-236`). No `is_empty` guard: the reference has
    // none on this axis (it does have one on the dismissal axis).
    let name = crate::artist_blacklist::get_all()
        .into_iter()
        .find(|a| a.artist_id == id)
        .map(|a| a.artist_name)
        .unwrap_or_else(|| qbz_i18n::t("Artist"));
    match crate::artist_blacklist::remove(id) {
        Ok(()) => {
            republish_all();
            emit_changed("artist", &artist_id, false);
            crate::toast_qt::success(qbz_i18n::t_args("{} removed from blacklist", &[&name]));
        }
        Err(e) => {
            log::error!("[qbz-qt] blacklist remove failed: {e}");
            crate::toast_qt::error(qbz_i18n::t("Failed to remove artist"));
        }
    }
}

/// Clear-All on the Artists tab (leaves the enabled flag + the albums axis).
/// No per-row settle signal: the reference has none either — an artist page
/// opened before the clear re-seeds on its next navigation.
pub fn clear_all_artists() {
    // Count BEFORE the wipe (`blacklist_manager.rs:251`); the toast reports it.
    let count = crate::artist_blacklist::count();
    match crate::artist_blacklist::clear_all() {
        Ok(()) => {
            republish_all();
            crate::toast_qt::success(qbz_i18n::tf(
                "Removed {} artist from blacklist",
                "Removed {} artists from blacklist",
                count as i64,
                &[&count.to_string()],
            ));
        }
        Err(e) => {
            log::error!("[qbz-qt] blacklist clear-all failed: {e}");
            crate::toast_qt::error(qbz_i18n::t("Failed to clear blacklist"));
        }
    }
}

/// Artist page ⋯ menu, Blacklist <-> Show. Direction comes from
/// `is_blacklisted_id_str` — which honours the enabled gate, exactly like the
/// reference's `is_blacklisted(id_num)`; the store's `INSERT OR REPLACE` makes
/// a redundant add idempotent. Settles through `blacklistChanged`
/// (`key == "artist:" + id`), which is also the rollback channel.
pub fn artist_toggle(artist_id: String, artist_name: String) {
    let Ok(id) = artist_id.parse::<u64>() else {
        log::debug!("[qbz-qt] blacklist toggle skipped for non-numeric id '{artist_id}'");
        return;
    };
    let was_blacklisted = crate::artist_blacklist::is_blacklisted_id_str(&artist_id);
    let res = if was_blacklisted {
        crate::artist_blacklist::remove(id)
    } else {
        crate::artist_blacklist::add(id, &artist_name, None)
    };
    let settled = match res {
        Ok(()) => {
            republish_all();
            // UNTRANSLATED on purpose — `main.rs:12799-12804` builds these two
            // with `format!`, and neither msgid exists in any catalog. See the
            // module docs.
            crate::toast_qt::success(if was_blacklisted {
                format!("{artist_name} is now visible")
            } else {
                format!("{artist_name} is now hidden")
            });
            !was_blacklisted
        }
        Err(e) => {
            log::error!("[qbz-qt] blacklist artist toggle failed: {e}");
            // Also untranslated in the reference (`main.rs:12813-12816`).
            crate::toast_qt::error("Failed to update artist visibility");
            was_blacklisted
        }
    };
    emit_changed("artist", &artist_id, settled);
}

/// Manager row click: open the artist page (`BlacklistActions.artist-select`).
pub fn open_artist(artist_id: String) {
    if artist_id.is_empty() {
        return;
    }
    crate::open_artist(artist_id);
}

// ---------------------------------------------------------------------------
// Actions — album axis
// ---------------------------------------------------------------------------

/// One-way block from a card / list-row ⋯ menu ("Block this album"). The source
/// grid drops the card on its next reload — there is no global observer, which
/// is the artist-block convention.
pub fn block_album(album_id: String, title: String, artist: String, cover_url: String) {
    if album_id.is_empty() {
        return;
    }
    match crate::artist_blacklist::add_album(&album_id, &title, &artist, &cover_url, None) {
        Ok(()) => {
            republish_all();
            emit_changed("album", &album_id, true);
            crate::toast_qt::success(qbz_i18n::t_args("Album \"{}\" blocked", &[&title]));
        }
        Err(e) => {
            log::error!("[qbz-qt] album block failed: {e}");
            crate::toast_qt::error(qbz_i18n::t("Failed to block album"));
        }
    }
}

/// Album page header ⋯ menu, Block <-> Unblock. Same optimistic-flip contract
/// as [`artist_toggle`], settled on `key == "album:" + id`.
pub fn album_toggle(album_id: String, title: String, artist: String, cover_url: String) {
    if album_id.is_empty() {
        return;
    }
    let was_blocked = crate::artist_blacklist::is_album_blacklisted(&album_id);
    let res = if was_blocked {
        crate::artist_blacklist::remove_album(&album_id)
    } else {
        crate::artist_blacklist::add_album(&album_id, &title, &artist, &cover_url, None)
    };
    let settled = match res {
        Ok(()) => {
            republish_all();
            crate::toast_qt::success(if was_blocked {
                qbz_i18n::t_args("Album \"{}\" unblocked", &[&title])
            } else {
                qbz_i18n::t_args("Album \"{}\" blocked", &[&title])
            });
            !was_blocked
        }
        Err(e) => {
            log::error!("[qbz-qt] album block toggle failed: {e}");
            crate::toast_qt::error(if was_blocked {
                qbz_i18n::t("Failed to unblock album")
            } else {
                qbz_i18n::t("Failed to block album")
            });
            was_blocked
        }
    };
    emit_changed("album", &album_id, settled);
}

/// Manager row `x` on the Albums tab: unblock one album (optimistic).
pub fn remove_album(album_id: String) {
    if album_id.is_empty() {
        return;
    }
    // Title captured BEFORE the delete (`blacklist_manager.rs:298-302`).
    let title = crate::artist_blacklist::get_all_albums()
        .into_iter()
        .find(|a| a.album_id == album_id)
        .map(|a| a.album_title)
        .unwrap_or_else(|| qbz_i18n::t("Album"));
    match crate::artist_blacklist::remove_album(&album_id) {
        Ok(()) => {
            republish_all();
            emit_changed("album", &album_id, false);
            crate::toast_qt::success(qbz_i18n::t_args("Album \"{}\" unblocked", &[&title]));
        }
        Err(e) => {
            log::error!("[qbz-qt] album remove failed: {e}");
            crate::toast_qt::error(qbz_i18n::t("Failed to unblock album"));
        }
    }
}

/// Clear-All on the Albums tab (leaves the enabled flag + the artists axis).
pub fn clear_all_albums() {
    // Count BEFORE the wipe (`blacklist_manager.rs:321`).
    let count = crate::artist_blacklist::album_count();
    match crate::artist_blacklist::clear_all_albums() {
        Ok(()) => {
            republish_all();
            crate::toast_qt::success(qbz_i18n::tf(
                "Removed {} album from blacklist",
                "Removed {} albums from blacklist",
                count as i64,
                &[&count.to_string()],
            ));
        }
        Err(e) => {
            log::error!("[qbz-qt] album clear-all failed: {e}");
            crate::toast_qt::error(qbz_i18n::t("Failed to clear album blacklist"));
        }
    }
}

/// Manager row click on the Albums tab: open the album page
/// (`BlacklistActions.album-select`).
pub fn open_album(album_id: String) {
    if album_id.is_empty() {
        return;
    }
    crate::open_album(album_id);
}

// ---------------------------------------------------------------------------
// Actions — reco-dismissal axis
// ---------------------------------------------------------------------------

/// "Not interested" on an artist card. The store only backfills an EMPTY
/// name/image, so a blank first write could never be repaired: when the card
/// could not supply a name we resolve it from the catalog first (the reference's
/// non-reco path), then persist and re-publish. Live rail backfill is out of
/// scope — the card leaves on the rail's next publish.
pub fn dismiss_artist(artist_id: String, artist_name: String, image_url: String) {
    let Ok(id) = artist_id.parse::<u64>() else {
        log::debug!("[qbz-qt] reco dismiss skipped for non-numeric id '{artist_id}'");
        return;
    };
    if !artist_name.is_empty() {
        crate::reco_dismiss_qt::dismiss(id, &artist_name, &image_url);
        // Leave the rails NOW, not on the next rebuild.
        crate::recommendations_qt::apply_dismissal(&artist_id);
        republish_all();
        // INFO, like the reference (`main.rs:12996-13002`).
        crate::toast_qt::info(qbz_i18n::t_args(
            "{} won't appear in Recommendations anymore",
            &[&artist_name],
        ));
        return;
    }
    // No display name on the card (search / home / pinned rail): resolve it
    // before persisting, or the manager row would render an empty line.
    let runtime = crate::app();
    crate::spawn(async move {
        let (name, image) = match runtime.core().get_artist(id).await {
            Ok(a) => (
                a.name,
                a.image.and_then(|i| i.best().cloned()).unwrap_or_default(),
            ),
            Err(e) => {
                log::warn!("[qbz-qt] reco dismiss name resolve failed for {id}: {e}");
                (String::new(), String::new())
            }
        };
        let image = if image.is_empty() { image_url } else { image };
        crate::reco_dismiss_qt::dismiss(id, &name, &image);
        // Same live drop as the named arm. This branch is the one a search or
        // pinned-rail card takes (no display name on the card), and it lands
        // one network round-trip later — the card must still go.
        crate::recommendations_qt::apply_dismissal(&id.to_string());
        republish_all();
        // Unresolvable name falls back to the generic line, exactly as the
        // reference's non-reco branch does (`main.rs:13026-13033`).
        crate::toast_qt::info(if name.is_empty() {
            qbz_i18n::t("Artist dismissed from Recommendations")
        } else {
            qbz_i18n::t_args("{} won't appear in Recommendations anymore", &[&name])
        });
    });
}

/// Recommendations tab row `x`: undo one dismissal. The artist becomes eligible
/// for the reco rails again on their next paint (the filter reads the store).
pub fn remove_dismissed(artist_id: String) {
    let Ok(id) = artist_id.parse::<u64>() else {
        log::debug!("[qbz-qt] reco undismiss skipped for non-numeric id '{artist_id}'");
        return;
    };
    // Name captured BEFORE the removal, with the reference's `is_empty` guard
    // on this axis (`blacklist_manager.rs:343-348`) — a row persisted without a
    // resolved name falls back to the generic "Artist".
    let name = crate::reco_dismiss_qt::list()
        .into_iter()
        .find(|a| a.artist_id == id)
        .map(|a| a.name)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| qbz_i18n::t("Artist"));
    crate::reco_dismiss_qt::remove(id);
    republish_all();
    // Unconditional: the store's `remove` is infallible (`:349-351`).
    crate::toast_qt::success(qbz_i18n::t_args("{} restored to Recommendations", &[&name]));
}

// ---------------------------------------------------------------------------
// Cover window dispatch
// ---------------------------------------------------------------------------

/// Albums-tab cover window: a JSON array of cover urls the view can currently
/// see. Disk hits emit immediately; misses download, then emit. Arrivals reuse
/// the existing `QbzLibrary.libraryArtworkReady(key, path)` with `key` = the
/// cover URL (same contract as the sidebar's url-keyed dispatch).
pub fn artwork_window(urls_json: String) {
    let urls: Vec<String> = serde_json::from_str(&urls_json).unwrap_or_default();
    log::debug!("[qbz-qt] blacklist artwork window: {} urls", urls.len());
    let mut missing: Vec<String> = Vec::new();
    for url in urls {
        let path = crate::artwork_qt::cached_path(&url);
        if path.is_empty() {
            missing.push(url);
        } else {
            emit_artwork(url, path);
        }
    }
    if missing.is_empty() {
        return;
    }
    crate::spawn(async move {
        let urls = missing;
        crate::artwork_qt::download_missing(urls.clone()).await;
        for url in urls {
            let path = crate::artwork_qt::cached_path(&url);
            if !path.is_empty() {
                emit_artwork(url, path);
            }
        }
    });
}

/// One `ui()` hop per domain: this signal belongs to `QbzLibrary`, so it goes
/// through the library bridge and never gets folded into a blacklist closure.
fn emit_artwork(key: String, path: String) {
    crate::library_bridge::ui(move |mut b| {
        b.as_mut()
            .library_artwork_ready(QString::from(key.as_str()), QString::from(path.as_str()));
    });
}
