//! Per-user library.db access — the local-only playlist flags, ported from
//! `crates/qbz/src/library_db.rs`. Opens
//! `<data_dir>/qbz/users/<uid>/library.db` on demand via the `qbz-library`
//! backend crate (ADR-006 — no glue copied).
//!
//! Why a LOCAL db and not the Qobuz API: the playlist heart is a qbz-only
//! flag. `/favorite/create` and `/favorite/delete` accept exactly five id
//! params — `album_ids`, `artist_ids`, `track_ids`, `label_ids`, `award_ids`
//! (inferred OpenAPI v10.0.0.0-beta, §Favorites) — there is no
//! `playlist_ids`, which is why the reference routes `("playlist",
//! "favorite")` to `db.set_playlist_favorite` instead (main.rs:13652, its
//! comment: "Qobuz /favorite/create rejects playlist_ids").
//!
//! `LibraryDatabase` holds a non-Send `rusqlite::Connection`, so every call
//! opens it fresh; async callers must wrap these in `spawn_blocking`.

use std::path::{Path, PathBuf};

use qbz_library::{LibraryDatabase, LibraryError};

/// `<data_dir>/qbz/users/<uid>/library.db` — the same per-user path the
/// reference uses, so the local organization data is shared between builds.
///
/// `unwrap_or(0)` — the GUEST profile, not a bail. This used to `?` out when
/// `last_user_id` was absent, which is the state of a machine that has never
/// completed a Qobuz login: for that user `db_path()` was permanently `None`,
/// so `with_db` answered `None` to every call and the ENTIRE local-playlist
/// feature (plus folders, local favourites and the mixtape tables) simply did
/// not exist — for exactly the population local playlists are FOR. It survived
/// review because every developer machine has logged in at least once.
///
/// `users/0/` is the designed home for that data, not an invention:
/// `AppRuntime::activate_offline` resolves its own user with the SAME
/// `load_last_user_id().unwrap_or(0)`, and `adopt_guest_profile` (#553)
/// renames `users/0/` onto the account on first login precisely so work done
/// logged-off follows the user in. The reference has no such hole because it
/// binds a session-scoped id instead (`library_db::set_user(user_id)` from
/// `init_shell_for_user`, which qbz/src/main.rs:172 documents as shared by the
/// ONLINE AND OFFLINE entries) — this is the port's substitution of
/// `load_last_user_id()` for that id, made to agree with it in the one case
/// where they diverged.
///
/// Reads still pass `create: false`, so nothing is conjured by listing; only a
/// write brings `users/0/library.db` into existence.
fn db_path() -> Option<PathBuf> {
    crate::local_state::db_path()
}

/// Run `f` against the per-user database. `create` mirrors the reference's
/// `open()`: writes must be able to bring the file into existence (a fresh
/// account has no library.db until the first local flag is set), reads must
/// NOT — an empty result is the right answer and creating the file as a side
/// effect of a read is a surprise.
///
/// `pub(crate)` because this is the port's only CREATE-CAPABLE accessor, and
/// MyQBZ needs it twice on a fresh account: `run_mixtape_migrations` at
/// session activation and the first `create_collection`. The other accessor,
/// `local_state::with_db` (already `pub`), is the right one for every
/// `local_*` read, but it returns `None` when library.db is absent
/// (`local_state.rs:44-46`) — through it a brand-new account could never run
/// the migrations, so it could never create a mixtape. Slint has no such
/// split: its `library_db::open` always creates
/// (`qbz/src/library_db.rs:54-64`).
pub(crate) fn with_db<F, R>(create: bool, f: F) -> Option<R>
where
    F: FnOnce(&LibraryDatabase) -> Result<R, LibraryError>,
{
    let store = qbz_library::LibraryStore::new(db_path()?);
    let result = if create {
        store.write(f).map(Some)
    } else {
        store.read(f)
    };
    match result {
        Ok(value) => value,
        Err(e) => {
            log::error!("[qbz-qt] library.db operation failed: {e}");
            None
        }
    }
}

/// Hearted playlist ids (favorites.rs `db.get_favorite_playlist_ids`).
/// Empty on any failure (missing user, missing db) — the Library sub-tab
/// then shows owned playlists only.
pub fn favorite_playlist_ids() -> Vec<u64> {
    with_db(false, |db| db.get_favorite_playlist_ids()).unwrap_or_default()
}

/// Same read, against an EXPLICIT per-user directory (`<…>/users/<uid>/`).
///
/// Session activation seeds `fav_cache_qt` from here, and at that moment
/// `UserDataPaths::load_last_user_id()` is not a safe way to name the user
/// being activated — `db_path()` above would resolve to whoever was persisted
/// last. The caller already holds the right directory, so it passes it.
///
/// Read-only, never creates the file: a fresh account with no local flags
/// yet is an empty set, not an error.
pub fn favorite_playlist_ids_at(base_dir: &Path) -> Vec<u64> {
    let path = base_dir.join("library.db");
    if !path.exists() {
        return Vec::new();
    }
    match LibraryDatabase::open(&path) {
        Ok(db) => db.get_favorite_playlist_ids().unwrap_or_else(|e| {
            log::warn!("[qbz-qt] library.db favorite playlist seed failed: {e}");
            Vec::new()
        }),
        Err(e) => {
            log::warn!("[qbz-qt] library.db open failed ({}): {e}", path.display());
            Vec::new()
        }
    }
}

/// Is this playlist hearted? Read straight from the db so the toggle picks
/// its direction from the authority, not from whatever a card happened to
/// render (main.rs `playlist_toggle_favorite_by_id`).
pub fn is_favorite_playlist(pid: u64) -> bool {
    with_db(false, |db| db.get_favorite_playlist_ids())
        .map(|ids| ids.contains(&pid))
        .unwrap_or(false)
}

/// Set the heart. Returns false when the write did not land (no user, open
/// or sqlite failure) so the caller can leave the UI alone.
pub fn set_favorite_playlist(pid: u64, favorite: bool) -> bool {
    with_db(true, |db| db.set_playlist_favorite(pid, favorite)).is_some()
}

/// Has this Qobuz playlist (by its SOURCE id) already been copied into the
/// user's library?
///
/// The reference seeds `PlaylistState.is-copied` from exactly this read when
/// the detail opens (`qbz/src/main.rs:4555`), which is what keeps the Copy
/// button hidden on the SECOND visit. The port used to carry `is_copied` in
/// the session document only, so a restart offered "Copy to your library"
/// again on a playlist that was already copied.
///
/// Read-only (`create: false`): a user with no local flags yet has no db, and
/// that is "not copied", not an error.
pub fn is_playlist_copied(pid: u64) -> bool {
    with_db(false, |db| db.is_playlist_copied(pid)).unwrap_or(false)
}

/// Record that a Qobuz playlist (by its SOURCE id) was copied into the user's
/// library. Idempotent server-side of the sqlite (`INSERT OR IGNORE`), so a
/// re-copy is a no-op. Returns false when the write did not land.
pub fn mark_playlist_copied(pid: u64) -> bool {
    with_db(true, |db| db.mark_playlist_copied(pid)).is_some()
}
