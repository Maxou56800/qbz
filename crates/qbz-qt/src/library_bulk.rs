//! Library (Favorites) BULK actions — the two multi-select bars.
//!
//! Reference: `crates/qbz/src/main.rs` `FavoritesActions::on_bulk_action`
//! (:22981, the Tracks tab) and `on_albums_bulk_action` (:22669, the Albums
//! tab in LIST mode only — owner 2026-07-24).
//!
//! Shape follows `local_bulk.rs`: the SELECTION lives in QML (it is a set of
//! rows the QML already holds), so `select-all` / `clear` never reach Rust;
//! every other action arrives as a JSON id array plus a scope word. Resolution
//! goes through the merged Library feed, which is this port's `FAV_CURRENT`.
//!
//! WHAT IS NOT OFFERED, and why — the bar must never render a control that
//! no-ops (the discipline `local_bulk.rs` set when it omitted rather than
//! shipped a dead toggle):
//!
//!  - **"Make available offline"** (`make-offline`, the Slint's
//!    `offline_cache::cache_tracks`). This port brings up no offline cache at
//!    all — `cast_qt.rs:32` and `local_playlist_qt.rs:345` both say so, and
//!    there is no `OfflineCacheState`, no download sink and no cached-id store
//!    to write into. The row is omitted from the Tracks bar rather than logged
//!    and dropped.
//!
//! Everything else the reference offers IS wired: play-next / play-later /
//! add-to-queue, add-to-playlist (the app-wide `QbzPlaylistPicker` modal, which
//! `local_bulk.rs` could not reach when it was written), add-to-mixtape
//! (`QbzMyQbzAdd`), and remove-from-Library.

use qbz_models::QueueTrack;

use crate::library_qt::{self, FeedItem};
use crate::myqbz_add_qt::AddItem;
use crate::playback_qt::{self, PlayContext};

/// QML entry point. `scope` = "track" | "album", `ids_json` = the JSON string
/// array the bar built from its own selection map, `action` = the bar's row id.
pub fn bulk_action(scope: String, ids_json: String, action: String) {
    let ids: Vec<String> = serde_json::from_str(&ids_json).unwrap_or_default();
    if ids.is_empty() {
        log::debug!("[qbz-qt] library bulk {action}: empty {scope} selection, ignored");
        return;
    }
    match scope.as_str() {
        "track" => tracks_action(ids, &action),
        "album" => albums_action(ids, &action),
        other => log::warn!("[qbz-qt] library bulk: unknown scope '{other}'"),
    }
}

// ---------------------------------------------------------------------------
// Resolution off the feed
// ---------------------------------------------------------------------------

/// The selected ids as feed rows, IN SELECTION ORDER (the order the ids
/// arrived, which is the order the bar's own model produced them in — the
/// visible order). A row that is not in the feed is dropped, same contract as
/// `library_qt::order_by_visible`.
fn rows_for(kind: &str, ids: &[String]) -> Vec<FeedItem> {
    library_qt::with_library(|d| {
        ids.iter()
            .filter_map(|id| {
                d.feed
                    .iter()
                    .find(|i| i.kind == kind && &i.id == id)
                    .cloned()
            })
            .collect::<Vec<_>>()
    })
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tracks bar
// ---------------------------------------------------------------------------

fn tracks_action(ids: Vec<String>, action: &str) {
    match action {
        "queue" | "play-next" | "play-later" => {
            let queue: Vec<QueueTrack> = rows_for("track", &ids)
                .iter()
                .filter_map(library_qt::feed_track_to_queue)
                .collect();
            if queue.is_empty() {
                log::info!("[qbz-qt] library bulk {action}: nothing resolvable in the selection");
                return;
            }
            // The bar's vocabulary vs the queue helper's: "play-next" -> "next"
            // (insert at the cursor, fed REVERSED so a multi-track insert keeps
            // its order), "play-later" -> "later" (append to the manual block's
            // tail), anything else appends.
            let mode = match action {
                "play-next" => "next",
                "play-later" => "later",
                _ => "queue",
            }
            .to_string();
            let runtime = crate::app();
            crate::spawn(async move {
                if let Err(e) = playback_qt::enqueue_track_list_mode(&runtime, queue, &mode).await {
                    log::error!("[qbz-qt] library bulk {mode} failed: {e}");
                }
            });
        }
        "add-to-playlist" => {
            // The picker takes decimal Qobuz track ids and filters the rest
            // itself; passing the SELECTION rather than the resolved rows keeps
            // it identical to the Slint's `playlist_picker::open_multi(&ids)`.
            let runtime = crate::app();
            crate::playlist_picker_qt::open_for_ids(&runtime, ids);
        }
        "add-to-mixtape" => {
            let items = mixtape_items(&rows_for("track", &ids));
            if items.is_empty() {
                return;
            }
            crate::myqbz_add_qt::open_items(items);
        }
        "remove-selected" => remove_favorites("track", ids),
        other => log::warn!("[qbz-qt] library bulk tracks: unknown action {other}"),
    }
}

/// Feed track rows -> MyQBZ payload items (`myqbz_add_qt::track_items_from_local`
/// twin for the Qobuz side).
///
/// `source` comes off the ROW, never a literal: the Tracks tab holds Qobuz rows
/// today, but a local/Plex row must never be stored as a Qobuz id — and
/// `source_from_str` maps anything that is not "local" back to Qobuz, so "plex"
/// has to be folded here (the same fold `LibraryView.qml`'s single-row
/// "Add to mixtape" does).
fn mixtape_items(rows: &[FeedItem]) -> Vec<AddItem> {
    rows.iter()
        .map(|item| AddItem {
            item_type: "track".to_string(),
            source: if item.source == "local" || item.source == "plex" {
                "local".to_string()
            } else {
                "qobuz".to_string()
            },
            source_item_id: item.id.clone(),
            title: item.title.clone(),
            subtitle: Some(item.artist.clone()),
            artwork_url: if item.image_url.is_empty() {
                None
            } else {
                Some(item.image_url.clone())
            },
            year: None,
            track_count: None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Albums bar (LIST mode only)
// ---------------------------------------------------------------------------

fn albums_action(ids: Vec<String>, action: &str) {
    match action {
        "queue" | "play-next" | "play-later" => {
            let mode = match action {
                "play-next" => "next",
                "play-later" => "later",
                _ => "queue",
            }
            .to_string();
            let runtime = crate::app();
            crate::spawn(async move {
                let mut queue: Vec<QueueTrack> = Vec::new();
                for id in &ids {
                    // Releases includes the same local/download/server groups
                    // as All. Resolve those keys locally before the catalog arm.
                    if library_qt::is_local_album_key(id) {
                        let key = id.clone();
                        let tracks = tokio::task::spawn_blocking(move || {
                            let mut rows = crate::local_albums::fetch_album_tracks_blocking(&key);
                            crate::local_playback::fill_missing_covers(&mut rows);
                            rows.iter()
                                .map(crate::local_playback::local_queue_track)
                                .collect()
                        })
                        .await
                        .unwrap_or_default();
                        queue.extend(playback_qt::stamped(tracks, PlayContext::album(id)));
                        continue;
                    }
                    match playback_qt::fetch_album_queue(&runtime, id).await {
                        Ok(tracks) => {
                            // Stamp EACH album's tracks with their own origin
                            // before concatenating: the "playing from" glyph
                            // must name the album the track came from, and
                            // `stamp_context` skips rows that already carry one,
                            // so the single stamp inside the enqueue helper
                            // cannot overwrite these.
                            queue.extend(playback_qt::stamped(tracks, PlayContext::album(id)));
                        }
                        Err(e) => log::error!("[qbz-qt] library bulk albums: {id}: {e}"),
                    }
                }
                if queue.is_empty() {
                    log::info!("[qbz-qt] library bulk albums {mode}: nothing resolvable");
                    return;
                }
                if let Err(e) = playback_qt::enqueue_track_list_mode(&runtime, queue, &mode).await {
                    log::error!("[qbz-qt] library bulk albums {mode} failed: {e}");
                }
            });
        }
        "remove-selected" => remove_favorites("album", ids),
        other => log::warn!("[qbz-qt] library bulk albums: unknown action {other}"),
    }
}

// ---------------------------------------------------------------------------
// Remove from Library
// ---------------------------------------------------------------------------

/// Un-favourite every selected row, then reload the Library.
///
/// Three things the reference does that are NOT optional here (main.rs:23040+):
///  - offline is read-only for hearts (spec 4.3) — refuse with an info toast
///    rather than firing writes that cannot land;
///  - an up-front toast on the TRACKS path, because a big selection takes
///    visible seconds (owner 2026-07-26). The albums path deliberately has
///    none — the reference does not toast there either, and the string is a
///    tracks-specific msgid, so inventing a generic one would add a
///    translation to all eight catalogues for a message upstream never shows;
///  - a 150 ms courtesy delay between calls, so a 200-row selection is not a
///    burst at Qobuz.
///
/// The feed row and the id cache are both updated per id (the same pair
/// `library_qt::toggle_favorite` writes), so every OTHER surface's heart
/// settles even before the reload lands.
fn remove_favorites(kind: &str, ids: Vec<String>) {
    if crate::offline_fwd::engine().status().is_offline() {
        crate::toast_qt::info(qbz_i18n::t("Not available offline"));
        return;
    }
    let kind = kind.to_string();
    let count = ids.len();
    if kind == "track" {
        let n = count.to_string();
        crate::toast_qt::info(qbz_i18n::t_args(
            "Removing {} tracks from favorites…",
            &[n.as_str()],
        ));
    }
    let runtime = crate::app();
    crate::spawn(async move {
        for id in &ids {
            if library_qt::is_local_feed_id(&kind, id) {
                if library_qt::is_favorite(&kind, id) {
                    library_qt::toggle_favorite(&runtime, &kind, id).await;
                }
                continue;
            }
            if let Err(e) = runtime.core().remove_favorite(&kind, id).await {
                log::error!("[qbz-qt] library bulk remove {kind}:{id} failed: {e}");
                continue;
            }
            crate::fav_cache_qt::set(&kind, id, false);
            crate::library_qt::set_feed_favorite(&kind, id, false);
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        log::info!("[qbz-qt] library bulk remove: {count} {kind}(s)");
        crate::reload_library();
    });
}
