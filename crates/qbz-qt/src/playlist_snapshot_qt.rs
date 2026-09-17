//! Persisted Qobuz playlist names and membership for Qt offline navigation.
//!
//! The schema and queries live in `qbz-library`; this file only joins those
//! frontend-neutral records to Qt's per-user database and ready-cache set.
//! Producers are detached because they consume data the online list/detail
//! loads already fetched and must never delay a render.

use std::collections::{HashMap, HashSet};

use qbz_library::qobuz_playlist_snapshot as repo;

pub use repo::AuthoritativeEntry;

/// Turn one `get_user_playlists()` response into authoritative snapshot
/// entries. Ownership comes from the session user id; when that id is not
/// known yet the caller must SKIP recording — an all-foreign list would both
/// starve the hydrator and start the two-generation retirement clock.
pub fn authoritative_entries(
    playlists: &[qbz_models::Playlist],
) -> Option<Vec<AuthoritativeEntry>> {
    if !crate::playlist_qt::session_user_known() {
        log::debug!(
            "[qbz-qt] playlist snapshot: session user unknown; authoritative list not recorded"
        );
        return None;
    }
    Some(
        playlists
            .iter()
            .map(|playlist| AuthoritativeEntry {
                qobuz_playlist_id: playlist.id,
                name: playlist.name.clone(),
                owner: Some(playlist.owner.name.clone()).filter(|owner| !owner.is_empty()),
                track_count: Some(playlist.tracks_count),
                remote_updated_at: playlist.updated_at,
                is_owned: crate::playlist_qt::owns(playlist.owner.id),
            })
            .collect(),
    )
}

pub fn record_authoritative_detached(mut entries: Vec<AuthoritativeEntry>) {
    let write = move || {
        let result = crate::library_qt::with_deleted_playlists(|deleted| {
            entries.retain(|entry| !deleted.contains(&entry.qobuz_playlist_id.to_string()));
            let result = crate::library_db_qt::with_db(true, |db| {
                Ok(db.with_connection(|conn| repo::record_authoritative_list(conn, &entries)))
            });
            result
        });
        match result {
            Some(Ok(generation)) => {
                log::debug!("[qbz-qt] playlist snapshot: authoritative generation {generation}");
                crate::playlist_index_qt::poke();
            }
            Some(Err(error)) => {
                log::warn!("[qbz-qt] playlist snapshot authoritative write failed: {error}")
            }
            None => {}
        }
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::spawn_blocking(write);
    } else {
        std::thread::spawn(write);
    }
}

pub fn record_detail_detached(playlist_id: u64, name: String, owner: String, track_ids: Vec<u64>) {
    let write = move || {
        let owner = Some(owner.as_str()).filter(|value| !value.is_empty());
        let result = crate::library_db_qt::with_db(true, |db| {
            Ok(db.with_connection(|conn| {
                repo::replace_tracks(conn, playlist_id, &name, owner, &track_ids)
            }))
        });
        match result {
            Some(Ok(true)) => crate::playlist_picker_qt::membership_index_progressed(),
            Some(Ok(false)) => log::debug!(
                "[qbz-qt] playlist snapshot: {playlist_id} is not in the user list; membership skipped"
            ),
            Some(Err(error)) => {
                log::warn!("[qbz-qt] playlist snapshot membership write failed: {error}")
            }
            None => {}
        }
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::spawn_blocking(write);
    } else {
        std::thread::spawn(write);
    }
}

/// playlist id -> (persisted name, point-in-time Qobuz track count).
pub fn headers_blocking() -> HashMap<u64, (String, Option<u32>)> {
    crate::library_db_qt::with_db(false, |db| Ok(db.with_connection(repo::all_headers)))
        .and_then(Result::ok)
        .map(|headers| {
            headers
                .into_iter()
                .map(|header| (header.qobuz_playlist_id, (header.name, header.track_count)))
                .collect()
        })
        .unwrap_or_default()
}

/// Persisted name for one playlist. This is the cold-start fallback for an
/// offline detail: the sidebar session cache may not have existed yet, while
/// the snapshot lives in the per-user library database.
pub fn name_blocking(playlist_id: u64) -> Option<String> {
    crate::library_db_qt::with_db(false, |db| {
        Ok(db.with_connection(|conn| repo::get_header(conn, playlist_id)))
    })
    .and_then(Result::ok)
    .flatten()
    .map(|header| header.name)
}

fn playable_membership(track_ids: Vec<u64>, cached: &HashSet<u64>) -> Vec<u64> {
    track_ids
        .into_iter()
        .filter(|track_id| cached.contains(track_id))
        .collect()
}

/// Playlists whose persisted membership contains at least one playable cache
/// entry. An expired subscription grace window makes the whole set empty.
pub fn available_offline_blocking() -> HashSet<u64> {
    if !crate::offline_fwd::offline_playback_allowed() {
        return HashSet::new();
    }
    let cached = crate::offline_qt::cached_ids_set();
    if cached.is_empty() {
        return HashSet::new();
    }
    crate::library_db_qt::with_db(false, |db| Ok(db.with_connection(repo::all_track_ids)))
        .and_then(Result::ok)
        .map(|memberships| {
            memberships
                .into_iter()
                .filter(|(_, ids)| ids.iter().any(|id| cached.contains(id)))
                .map(|(playlist_id, _)| playlist_id)
                .collect()
        })
        .unwrap_or_default()
}

/// One playlist's playable snapshot membership in its persisted position
/// order. Non-downloaded Qobuz rows are intentionally hidden offline; local
/// and reachable-Plex sidecars are merged by `local_playlist_qt`.
pub fn playable_track_ids_blocking(playlist_id: u64) -> Vec<u64> {
    if !crate::offline_fwd::offline_playback_allowed() {
        return Vec::new();
    }
    let cached = crate::offline_qt::cached_ids_set();
    if cached.is_empty() {
        return Vec::new();
    }
    crate::library_db_qt::with_db(false, |db| {
        Ok(db.with_connection(|conn| repo::track_ids(conn, playlist_id)))
    })
    .and_then(Result::ok)
    .map(|track_ids| playable_membership(track_ids, &cached))
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playable_membership_preserves_snapshot_order_and_repetitions() {
        let cached = HashSet::from([3, 7]);
        assert_eq!(
            playable_membership(vec![9, 7, 3, 7, 4], &cached),
            vec![7, 3, 7]
        );
    }

    #[test]
    fn playable_membership_hides_every_uncached_row() {
        let cached = HashSet::from([42]);
        assert!(playable_membership(vec![1, 2, 3], &cached).is_empty());
    }
}
