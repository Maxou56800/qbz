//! First-class LOCAL playlists — port of `crates/qbz/src/local_playlist.rs`.
//!
//! # Nothing here is a new store
//!
//! The data lives where it already lives: `qbz_library::local_playlists`, in
//! the SAME per-user `library.db` the reference build writes, under the same
//! `local:<uuid>` ids. `LibraryDatabase::open()` runs that module's
//! `init_schema` itself (`qbz-library/src/database.rs:53`) — all
//! `CREATE TABLE IF NOT EXISTS` plus pragma-guarded additive `ALTER`s — and
//! this port opens through the same call. So a user who already has local
//! playlists sees them here with no migration, and can keep using either
//! build against the same rows.
//!
//! That is the whole point: local playlists are the feature for people
//! running QBZ as a player WITHOUT Qobuz, so losing or forking their data in
//! the Slint -> Qt move is the one outcome that is not acceptable.
//!
//! # Scope of this file
//!
//! The repo layer, write paths, detail/playback/reorder flows, offline Qobuz
//! subsets and the sidebar's cover resolution.
//!
//! OWED PORTS (recorded, not silently dropped):
//! - `resolve_cover_urls`'s OFFLINE-CACHE arm: the reference fills leftover
//!   collage slots from downloaded Qobuz covers. Qt now resolves that metadata
//!   for detail rows, but this synchronous sidebar helper still does not enter
//!   the async cache lock; a Qobuz-only local playlist can therefore fall back
//!   to the glyph in the sidebar.
//! - `add_drag_tracks_blocking`: the sidebar drop payload. The drag seam
//!   exists in this port; wiring it is part of the sidebar step.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use qbz_library::local_playlists as repo;
use qbz_models::QueueTrack;

use crate::library_db_qt::with_db;
use crate::playlist_qt::{PlaylistDoc, PlaylistTrackRow};

type Runtime = Arc<AppRuntime<LoggingAdapter>>;

/// Type guard (D7): a playlist reference is EITHER a Qobuz `u64` id or a
/// `local:<uuid>` string. Qobuz-bound calls take `u64` only, so a local ref is
/// unrepresentable there BY CONSTRUCTION — which is what stops a `local:` id
/// from ever reaching an endpoint that would read it as a catalog number.
///
/// It is the ONE place that classification happens. Every playlist mutation
/// used to hand-roll it as `is_local_id(id)` then a separate `parse::<u64>()`,
/// nine times — correct at each site, but it left "a ref classified by the
/// wrong half" representable, and the id-space hazard behind it has already
/// produced real bugs here (a LocalLibrary row id is a small integer that
/// parses as a perfectly valid Qobuz id, it just means a different track).
/// A new call site gets the guard for free; it cannot forget the test.
#[derive(Debug, Clone)]
pub enum PlaylistRef {
    Qobuz(u64),
    Local(String),
}

impl PlaylistRef {
    pub fn parse(id: &str) -> Option<Self> {
        if repo::is_local_playlist_id(id) {
            Some(Self::Local(id.to_string()))
        } else {
            id.parse::<u64>().ok().map(Self::Qobuz)
        }
    }
}

/// True when `id` names a local playlist.
pub fn is_local_id(id: &str) -> bool {
    repo::is_local_playlist_id(id)
}

// ──────────────────────── blocking repo wrappers ────────────────────────
// Every one opens the per-user library.db on the CALLING thread — never call
// these from the Qt event loop; wrap them in `spawn_blocking`.
//
// Reads pass `create: false` so listing playlists on a fresh account cannot
// conjure a library.db as a side effect; writes pass `true` because "create
// your first local playlist" must work on an install that has never written
// a local flag before.

pub fn list_blocking() -> Vec<repo::LocalPlaylist> {
    with_db(false, |db| Ok(db.with_connection(repo::list)))
        .and_then(|r| r.ok())
        .unwrap_or_default()
}

pub fn get_blocking(id: &str) -> Option<repo::LocalPlaylist> {
    with_db(false, |db| {
        Ok(db.with_connection(|conn| repo::get(conn, id)))
    })
    .and_then(|r| r.ok())
    .flatten()
}

pub fn get_tracks_blocking(id: &str) -> Vec<repo::LocalPlaylistTrack> {
    with_db(false, |db| {
        Ok(db.with_connection(|conn| repo::get_tracks(conn, id)))
    })
    .and_then(|r| r.ok())
    .unwrap_or_default()
}

pub fn create_blocking(
    name: &str,
    description: Option<&str>,
    offline_only: bool,
) -> Option<String> {
    with_db(true, |db| {
        Ok(db.with_connection(|conn| repo::create(conn, name, description, offline_only)))
    })
    .and_then(|r| r.ok())
}

pub fn update_blocking(
    id: &str,
    name: &str,
    description: Option<&str>,
    offline_only: bool,
) -> bool {
    with_db(true, |db| {
        Ok(db.with_connection(|conn| {
            repo::rename(conn, id, name)?;
            repo::set_description(conn, id, description)?;
            repo::set_offline_only(conn, id, offline_only)
        }))
    })
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

pub fn delete_blocking(id: &str) -> bool {
    with_db(true, |db| {
        Ok(db.with_connection(|conn| repo::delete(conn, id)))
    })
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// B3: the manager's favorite flag for a local playlist. Locals keep their
/// own flag on the `local_playlists` row — they are not in `playlist_settings`
/// (that table's PK is a u64 Qobuz id, which a `local:<uuid>` can never be).
pub fn set_favorite_blocking(id: &str, favorite: bool) -> bool {
    with_db(true, |db| {
        Ok(db.with_connection(|conn| repo::set_favorite(conn, id, favorite)))
    })
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// B3: hidden flag for a local playlist (hidden locals drop from the sidebar).
pub fn set_hidden_blocking(id: &str, hidden: bool) -> bool {
    with_db(true, |db| {
        Ok(db.with_connection(|conn| repo::set_hidden(conn, id, hidden)))
    })
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// Move a local playlist into `folder_id`, or to root when `None`. Folder
/// membership for locals rides the `local_playlists.folder_id` column, which
/// points at the SAME `playlist_folders` table the Qobuz playlists use — so
/// one folder holds both kinds.
pub fn move_to_folder_blocking(id: &str, folder_id: Option<&str>) {
    with_db(true, |db| {
        Ok(db.with_connection(|conn| repo::move_to_folder(conn, id, folder_id)))
    });
}

// ──────────────────────────── track appends ────────────────────────────

/// Append Qobuz track ids. Returns the inserted count.
///
/// Ids in the Plex synthetic namespace (>= 2^40, `local_plex::PLEX_TRACK_ID_FLOOR`)
/// are NOT catalog ids: storing one writes a row that can never resolve — the
/// field-garbage class the reference calls out. They are refused HERE, at the
/// last gate before the repo write, rather than at each caller.
pub fn add_qobuz_tracks_blocking(id: &str, track_ids: &[u64]) -> usize {
    let entries: Vec<repo::LocalPlaylistTrackInput> = track_ids
        .iter()
        .filter(|&&tid| {
            if tid >= crate::local_plex::PLEX_TRACK_ID_FLOOR {
                log::warn!(
                    "[qbz-qt] local playlist add: refused non-catalog id {tid} as a Qobuz ref"
                );
                false
            } else {
                true
            }
        })
        .map(|&tid| repo::LocalPlaylistTrackInput::Qobuz(tid))
        .collect();
    add_inputs_blocking(id, &entries)
}

/// Resolve a `local_tracks` row to its playlist input, SOURCE-AWARE: an
/// offline copy (`qobuz_download`) becomes a Qobuz ref (it has a real catalog
/// id), a Plex row becomes a Plex ref (its rating key lives in `file_path`),
/// anything else becomes a local file path. Getting this wrong is how a row
/// ends up unresolvable forever.
fn local_row_input(
    db: &qbz_library::LibraryDatabase,
    rid: i64,
) -> Result<Option<repo::LocalPlaylistTrackInput>, qbz_library::LibraryError> {
    let Some(track) = db.get_track(rid)? else {
        log::warn!("[qbz-qt] local playlist add: unknown local row {rid}");
        return Ok(None);
    };
    Ok(Some(match track.source.as_deref() {
        Some("qobuz_download") => match track.qobuz_track_id {
            Some(qid) => repo::LocalPlaylistTrackInput::Qobuz(qid as u64),
            None => repo::LocalPlaylistTrackInput::Local(track.file_path.clone()),
        },
        Some("plex") => repo::LocalPlaylistTrackInput::Plex(track.file_path.clone()),
        _ => repo::LocalPlaylistTrackInput::Local(track.file_path.clone()),
    }))
}

fn add_inputs_blocking(id: &str, entries: &[repo::LocalPlaylistTrackInput]) -> usize {
    if entries.is_empty() {
        return 0;
    }
    with_db(true, |db| {
        Ok(db.with_connection(|conn| repo::add_tracks(conn, id, entries)))
    })
    .and_then(|r| r.ok())
    .unwrap_or(0)
}

/// Append local-mode refs — `"<i64>"` LocalLibrary row ids (resolved
/// source-aware through [`local_row_input`]) or `"plex:<rating key>"` Plex
/// rows. Plex rows carry the KEY rather than a row id because their synthetic
/// ids never resolve through `get_track`. Returns the inserted count.
pub fn add_local_refs_blocking(id: &str, refs: &[String]) -> usize {
    let entries: Vec<repo::LocalPlaylistTrackInput> = with_db(true, |db| {
        let mut out = Vec::new();
        for r in refs {
            if let Some(key) = r.strip_prefix("plex:") {
                out.push(repo::LocalPlaylistTrackInput::Plex(key.to_string()));
            } else if let Some(item) = r.strip_prefix("jellyfin:") {
                out.push(repo::LocalPlaylistTrackInput::Jellyfin(item.to_string()));
            } else if let Some(item) = r.strip_prefix("subsonic:") {
                out.push(repo::LocalPlaylistTrackInput::Subsonic(item.to_string()));
            } else if let Ok(rid) = r.parse::<i64>() {
                if let Some(input) = local_row_input(db, rid)? {
                    out.push(input);
                }
            } else {
                log::warn!("[qbz-qt] local playlist add: unrecognized ref {r}");
            }
        }
        Ok(out)
    })
    .unwrap_or_default();
    add_inputs_blocking(id, &entries)
}

/// Resolve carried local-mode refs into the TYPED membership refs the
/// containment index takes (`qbz_library::playlist_membership`). Shares
/// [`local_row_input`]'s worldview: a Plex ref is its rating key, and a
/// library row contributes its path plus — for offline copies — its catalog
/// id. An unresolvable row id still yields a ref (row-id-only), so sidecar
/// membership can answer even when the row vanished from the library.
pub fn membership_refs_blocking(
    refs: &[String],
) -> Vec<qbz_library::playlist_membership::PlaylistTrackRef> {
    use qbz_library::playlist_membership::PlaylistTrackRef;
    with_db(false, |db| {
        let mut out = Vec::new();
        for r in refs {
            if let Some(key) = r.strip_prefix("plex:") {
                out.push(PlaylistTrackRef::Plex {
                    rating_key: key.to_string(),
                });
            } else if let Some(item) = r.strip_prefix("jellyfin:") {
                out.push(PlaylistTrackRef::Remote {
                    source: "jellyfin".to_string(),
                    item_id: item.to_string(),
                });
            } else if let Some(item) = r.strip_prefix("subsonic:") {
                out.push(PlaylistTrackRef::Remote {
                    source: "subsonic".to_string(),
                    item_id: item.to_string(),
                });
            } else if let Ok(rid) = r.parse::<i64>() {
                out.push(match db.get_track(rid)? {
                    Some(track) => PlaylistTrackRef::from_library_row(
                        track.source.as_deref().unwrap_or("local"),
                        rid,
                        &track.file_path,
                        track.qobuz_track_id.map(|v| v as u64),
                    ),
                    None => PlaylistTrackRef::Library {
                        track_id: rid,
                        path: None,
                        qobuz_track_id: None,
                    },
                });
            }
        }
        Ok(out)
    })
    .unwrap_or_default()
}

// ──────────────────────────── custom artwork ────────────────────────────

// There is deliberately NO `set_custom_artwork_blocking` twin of the clear
// below. Writing this column was the port's original plan and it was never
// called: the cover menu persists to `custom_playlist_covers.json` through
// `cover_artwork_qt`, which is the ONE store both the sidebar and this
// module's doc now read. A second writer would recreate exactly the split
// that made a freshly picked cover invisible on its own page.

/// Clear a local playlist's custom artwork from the `library.db` column.
///
/// Kept, and called from `cover_artwork_qt::remove_custom_playlist_cover`,
/// purely so "Remove cover" can clear a cover an EARLIER build stored here —
/// the doc still reads this column as a fallback. Nothing writes it.
/// Blocking.
pub fn clear_custom_artwork_blocking(id: &str) {
    with_db(true, |db| {
        Ok(db.with_connection(|conn| repo::set_custom_artwork(conn, id, None)))
    });
}

// ──────────────────────────── cover resolution ────────────────────────────

/// Up to `limit` cover refs for a local playlist's tracks, in track order and
/// WITHOUT any network — the sidebar / manager micro-collage.
///
/// Sources, all local: a Local track's `local_tracks.artwork_path`, and a Plex
/// track's cached thumb. Returns file paths and Plex thumb paths; the art
/// loader routes by shape.
///
/// Blocking — call it from `spawn_blocking`. The reference is async because
/// its leftover-slot fill enters the offline cache; this synchronous helper
/// deliberately does not, while the detail loader below does.
pub fn resolve_cover_urls_blocking(id: &str, limit: usize) -> Vec<String> {
    let mut covers: Vec<String> = Vec::new();
    for t in get_tracks_blocking(id) {
        if covers.len() >= limit {
            break;
        }
        match t.source {
            repo::LocalPlaylistTrackSource::Local => {
                if let Some(path) = t.local_path {
                    if let Some(Some(track)) = with_db(false, |db| db.get_track_by_path(&path)) {
                        if let Some(art) = track.artwork_path {
                            if !covers.contains(&art) {
                                covers.push(art);
                            }
                        }
                    }
                }
            }
            repo::LocalPlaylistTrackSource::Plex => {
                if let Some(key) = t.plex_key {
                    if let Ok(list) = qbz_plex::plex_cache_get_cached_tracks_by_keys(&[key]) {
                        if let Some(pt) = list.into_iter().next() {
                            let lt = crate::local_plex::map_cached_to_local_track(pt);
                            if let Some(art) = lt.artwork_path {
                                if !covers.contains(&art) {
                                    covers.push(art);
                                }
                            }
                        }
                    }
                }
            }
            repo::LocalPlaylistTrackSource::Jellyfin | repo::LocalPlaylistTrackSource::Subsonic => {
                if let Some(item) = t.local_path {
                    let source = t.source.as_str().to_string();
                    let resolved = crate::media_servers_qt::tracks_by_item_ids_blocking(&[(
                        source.clone(),
                        item.clone(),
                        0,
                    )]);
                    if let Some(art) = resolved
                        .get(&(source, item))
                        .and_then(|lt| lt.artwork_path.clone())
                    {
                        if !covers.contains(&art) {
                            covers.push(art);
                        }
                    }
                }
            }
            // Qobuz rows would fill their slot from the async offline cache in
            // the reference; this blocking sidebar helper cannot enter it.
            repo::LocalPlaylistTrackSource::Qobuz => {}
        }
    }
    covers.truncate(limit);
    covers
}

// ──────────────────────── the detail view ────────────────────────

/// One resolved, renderable row.
///
#[derive(Clone)]
pub enum RowItem {
    /// Full catalog track (online fetch).
    Qobuz(Box<qbz_models::Track>),
    /// Qobuz metadata from the persistent offline-cache index. Only Ready
    /// rows become this variant; uncached snapshot members stay hidden.
    Cached {
        track_id: u64,
        title: String,
        artist: String,
        album: String,
        duration_secs: u64,
        bit_depth: Option<u32>,
        sample_rate: Option<f64>,
        artwork_path: Option<String>,
    },
    /// Local file resolved from library.db by path.
    Local(Box<qbz_library::LocalTrack>),
    /// A local file whose metadata lookup missed but whose file EXISTS on
    /// disk — renders with a filename fallback rather than vanishing. Hiding
    /// is for rows with no metadata source anywhere, not for a file that is
    /// sitting right there.
    LocalFile { path: String },
    /// Plex ref resolved from the Plex cache into the same `LocalTrack` shape
    /// the Local Library merges, so render / queue / artwork all ride the
    /// existing source-aware paths.
    Plex(Box<qbz_library::LocalTrack>),
    /// A ref that cannot resolve right now: a `plex_key` the cache does not
    /// know (purged, never synced, or garbage written by an old mis-typed
    /// add), or a `qobuz_track_id` outside the catalog range (the legacy
    /// untyped-drag bug stored Plex synthetic 2^40 ids as Qobuz ids).
    /// Rendered HONESTLY and still selectable, so the user can remove it.
    Unresolved {
        /// "plex" (a cache miss — may heal after a resync) or "qobuz"
        /// (an out-of-range id — permanent garbage).
        kind: &'static str,
        /// The raw stored ref, shown so the user knows WHAT is broken.
        reference: String,
    },
}

#[derive(Clone)]
pub struct LoadedRow {
    pub position: i32,
    pub item: RowItem,
}

async fn ready_cached_rows(track_ids: &[u64]) -> HashMap<u64, RowItem> {
    if track_ids.is_empty() {
        return HashMap::new();
    }
    let Some(offline) = crate::offline_qt::get().await else {
        return HashMap::new();
    };
    let cache_path = offline.get_cache_path();
    let guard = offline.db.lock().await;
    let Some(db) = guard.as_ref() else {
        return HashMap::new();
    };
    let mut cached = HashMap::new();
    for track_id in track_ids {
        let Ok(Some(info)) = db.get_track(*track_id) else {
            continue;
        };
        if !matches!(info.status, qbz_offline_cache::OfflineCacheStatus::Ready) {
            continue;
        }
        let artwork_path = info.resolve_cover_path(&cache_path);
        cached.insert(
            *track_id,
            RowItem::Cached {
                track_id: info.track_id,
                title: info.title,
                artist: info.artist,
                album: info.album.unwrap_or_default(),
                duration_secs: info.duration_secs,
                bit_depth: info.bit_depth,
                sample_rate: info.sample_rate,
                artwork_path,
            },
        );
    }
    cached
}

/// The open local detail's playable queue snapshot, aligned with the row ids,
/// plus the per-row repo positions used for removal. Mirrors what
/// `playlist_qt` keeps for Qobuz lists.
static CURRENT_QUEUE: LazyLock<Mutex<Vec<QueueTrack>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// (playlist id, offline_only) of the open local detail.
static CURRENT_META: LazyLock<Mutex<Option<(String, bool)>>> = LazyLock::new(|| Mutex::new(None));
/// Row display id -> repo `position`, for removal.
static ROW_POSITIONS: LazyLock<Mutex<HashMap<String, i32>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Clear the open-detail snapshot, so rows from a previously open playlist can
/// never resolve against a different one.
pub fn clear_open_snapshot() {
    if let Ok(mut cur) = CURRENT_QUEUE.lock() {
        cur.clear();
    }
    if let Ok(mut meta) = CURRENT_META.lock() {
        *meta = None;
    }
    if let Ok(mut pos) = ROW_POSITIONS.lock() {
        pos.clear();
    }
}

/// The id of the open local detail, if one is open.
///
/// Since Seam B (mixed Qobuz details adopt this snapshot too — see
/// [`set_open_mixed_snapshot`]) this is `Some` for TWO different kinds of open
/// page, and the difference matters at four call sites. Read the note on
/// [`local_detail_open`] before routing on it.
pub fn open_id() -> Option<String> {
    CURRENT_META.lock().ok()?.as_ref().map(|(id, _)| id.clone())
}

/// True only while a FIRST-CLASS LOCAL detail (`local:<uuid>`) is the open
/// page — not a mixed Qobuz one.
///
/// [`open_id`] used to answer exactly that question, because the snapshot could
/// only ever hold a local playlist. Seam B broke the equivalence: a mixed Qobuz
/// detail now adopts the same snapshot (it has to — its rows are the same
/// blend of catalog, file and Plex tracks, and they are resolved by the same
/// helpers), so `open_id()` is `Some("123456")` on a page that is still, for
/// every write, a Qobuz playlist.
///
/// The split is deliberate and it follows the reference, which never routed on
/// this snapshot at all: Slint asks three separate predicates
/// (`is_local() || offline_subset() || playlist::is_mixed()`, main.rs:13531)
/// and picks a different one per action. Here that becomes: PLAYBACK routes on
/// `open_id()` — both kinds play from the merged queue, which is the whole
/// point — while REMOVE and REORDER route on this, because a mixed playlist's
/// writes go to Qobuz and to the sidecar tables, never to `local_playlists`.
/// Sending them to [`remove_row`] would look right and silently write nothing:
/// the repo has no row for a Qobuz playlist id.
pub fn local_detail_open() -> bool {
    open_id().map(|id| is_local_id(&id)).unwrap_or(false)
}

/// Read + resolve a QOBUZ playlist's SIDECAR rows (`playlist_local_tracks` +
/// `playlist_plex_tracks`) with their stored absolute positions — the shared
/// reader behind the ONLINE mixed detail (`playlist_qt::load`). Port of
/// `crates/qbz/src/local_playlist.rs:520-587`.
///
/// Runs the one-shot position healing first (Seam C): collided slots — the
/// legacy 0-based picker/drag writes, and create-and-add's parallel 0-based
/// local+plex rows — renumber stably into the append region. Drift alone is
/// never touched (E7). Healing is BEST-EFFORT: the interleave tolerates
/// collisions (same-slot rows all emit), so a failure logs and reading
/// proceeds.
///
/// Plex refs resolve from the Plex cache in ONE bulk lookup; a miss renders the
/// honest `Unresolved` row (E8) instead of vanishing — a row the user put there
/// that silently disappears is indistinguishable from data loss.
///
/// Returned rows are local-table-first then plex, each position ASC — the
/// stable claim order the interleave's same-slot emit relies on (E1/E2).
///
/// BLOCKING — call it on a worker thread.
pub fn read_sidecar_rows_blocking(
    playlist_id: u64,
    qobuz_track_count: u32,
    include_plex: bool,
) -> Vec<LoadedRow> {
    let (mut rows, plex_refs, remote_refs) = with_db(false, |db| {
        match db.heal_playlist_sidecar_positions(playlist_id, qobuz_track_count) {
            Ok(healed) => {
                for entry in &healed {
                    log::warn!(
                        "[qbz-qt] playlist {playlist_id}: healed sidecar position collision — {entry}"
                    );
                }
            }
            Err(e) => {
                log::warn!("[qbz-qt] playlist {playlist_id}: sidecar healing failed: {e}");
            }
        }
        let rows: Vec<LoadedRow> = db
            .get_playlist_local_tracks_with_position(playlist_id)?
            .into_iter()
            .map(|r| LoadedRow {
                position: r.playlist_position,
                item: RowItem::Local(Box::new(r.track)),
            })
            .collect();
        let plex_refs: Vec<(String, i32)> = if include_plex {
            db.get_playlist_plex_tracks_with_position(playlist_id)?
        } else {
            Vec::new()
        };
        let remote_refs = db.get_playlist_remote_tracks_with_position(playlist_id)?;
        Ok((rows, plex_refs, remote_refs))
    })
    .unwrap_or_default();

    if !plex_refs.is_empty() {
        let keys: Vec<String> = plex_refs.iter().map(|(key, _)| key.clone()).collect();
        let resolved: HashMap<String, qbz_library::LocalTrack> =
            match qbz_plex::plex_cache_get_cached_tracks_by_keys(&keys) {
                Ok(list) => list
                    .into_iter()
                    .map(crate::local_plex::map_cached_to_local_track)
                    // Keyed by `file_path`, which is where the Plex merge
                    // stores the rating key (local_plex.rs:276) — the same
                    // convention `local_picker_ref_for_track` reads back.
                    .map(|t| (t.file_path.clone(), t))
                    .collect(),
                Err(e) => {
                    log::warn!("[qbz-qt] playlist {playlist_id}: plex cache resolve failed: {e}");
                    HashMap::new()
                }
            };
        rows.extend(plex_refs.into_iter().map(|(key, position)| LoadedRow {
            position,
            item: match resolved.get(&key) {
                Some(track) => RowItem::Plex(Box::new(track.clone())),
                None => {
                    log::warn!(
                        "[qbz-qt] playlist {playlist_id}: plex key {key:?} not in the Plex cache \
                         — rendered as unavailable"
                    );
                    RowItem::Unresolved {
                        kind: "plex",
                        reference: key,
                    }
                }
            },
        }));
    }

    // Jellyfin/Subsonic sidecars — the Plex pattern against the media cache.
    // A miss renders the honest Unresolved row, never a vanished one.
    if !remote_refs.is_empty() {
        let resolved = crate::media_servers_qt::tracks_by_item_ids_blocking(&remote_refs);
        rows.extend(
            remote_refs
                .into_iter()
                .map(|(source, item, position)| LoadedRow {
                    position,
                    item: match resolved.get(&(source.clone(), item.clone())) {
                        Some(track) => RowItem::Local(Box::new(track.clone())),
                        None => {
                            log::warn!(
                                "[qbz-qt] playlist {playlist_id}: {source} item {item:?} not in \
                                 the media cache — rendered as unavailable"
                            );
                            RowItem::Unresolved {
                                kind: if source == "jellyfin" {
                                    "jellyfin"
                                } else {
                                    "subsonic"
                                },
                                reference: item,
                            }
                        }
                    },
                }),
        );
    }
    rows
}

/// Adopt the ONLINE mixed Qobuz detail's merged queue snapshot into the
/// open-detail statics this module owns, so `play` / `play_shuffled` /
/// `local_picker_ref_for_row` work over the merged rows exactly like a local
/// detail does (row identity E11). Port of `local_playlist.rs:599-612`.
///
/// `offline_only` is always false here — a real Qobuz playlist never stamps the
/// D8 guard; excluding the local/Plex rows from a QConnect push happens
/// per-track at admission, off `QueueTrack.source`.
///
/// It DOES write `CURRENT_META`, like the reference, and that is what makes the
/// three playback routes serve a mixed detail with no change of their own. The
/// two routes that must NOT follow it read [`local_detail_open`] instead.
pub fn set_open_mixed_snapshot(
    playlist_id: &str,
    queue: Vec<QueueTrack>,
    positions: HashMap<String, i32>,
) {
    if let Ok(mut cur) = CURRENT_QUEUE.lock() {
        *cur = queue;
    }
    if let Ok(mut meta) = CURRENT_META.lock() {
        *meta = Some((playlist_id.to_string(), false));
    }
    if let Ok(mut pos) = ROW_POSITIONS.lock() {
        *pos = positions;
    }
}

// `queue_track_for_row` and `plex_key_for_row` lived here and are GONE: they
// read the open-detail queue snapshot to build a playable QueueTrack, which is
// what `playlist_qt::row_to_queue` already does for BOTH kinds of playlist —
// and does better, because a local detail adopts its rows into that shared
// page (`adopt_doc`) and `row_to_queue` types them from `row.source`, the
// guard that keeps a library rowid off the QConnect wire as a catalog id.
// `CURRENT_QUEUE` stays: `local_picker_ref_for_row` below is its live reader.

/// Local-mode picker ref for an open-detail row: `"plex:<key>"` for resolved
/// Plex rows, `"<library row id>"` for local file rows and for OFFLINE-CACHE
/// rows, `None` for Qobuz rows (those ride the catalog-id flow).
pub fn local_picker_ref_for_row(id: &str) -> Option<String> {
    let queue = CURRENT_QUEUE.lock().ok()?;
    let q = queue.iter().find(|q| q.id.to_string() == id)?;
    local_picker_ref_for_queue_track(q)
}

/// The picker ref for a QUEUE row, source-aware like
/// `local_picker_ref_for_track`: Plex and the media servers ride the key
/// their row keeps in the hint slot (the rating key / the server item id —
/// `local_playback::local_queue_track`, `media_servers_qt::cached_to_local_track`),
/// an offline copy its library row id (ALSO in the hint: its queue id is the
/// catalog id when the copy has one, which is what `local_row_input` needs to
/// re-derive the right input kind), a local file its row id. `None` = a
/// local row this port cannot file (a media row that lost its key). The
/// queue's "save as playlist" and per-row "Add to playlist" build their local
/// refs here — never by hand (the module header's rule).
pub fn local_picker_ref_for_queue_track(q: &QueueTrack) -> Option<String> {
    match q.source.as_deref() {
        Some("plex") => q
            .source_item_id_hint
            .as_ref()
            .map(|key| format!("plex:{key}")),
        Some(source @ ("jellyfin" | "subsonic")) => q
            .source_item_id_hint
            .as_ref()
            .map(|item| format!("{source}:{item}")),
        Some("local") => Some(q.id.to_string()),
        Some("qobuz_download") => q.source_item_id_hint.clone(),
        _ => None,
    }
}

/// Local-mode picker ref for a LOCAL LIBRARY row (`main.rs:4701`
/// `local_picker_ref`) — the sibling of [`local_picker_ref_for_row`], which
/// serves the open local-playlist DETAIL instead.
///
/// Two different sources, two helpers, and they are not interchangeable: a
/// detail row only knows its display id and recovers the Plex key from the
/// open detail's queue snapshot, while a `LocalTrack` carries the key in
/// `file_path` (that is where the Plex merge stores it) and has a real
/// library row id. A Plex row must NEVER ride its numeric id — the synthetic
/// 2^40 ids do not resolve through `get_track`, and typing one as a Qobuz id
/// is the legacy bug that wrote permanently unresolvable rows.
///
/// Jellyfin/Subsonic rows likewise NEVER ride their numeric id — it is a
/// media-cache rowid, which `local_row_input`'s `get_track` would happily
/// resolve against `local_tracks` (the same number, a DIFFERENT track). Like
/// Plex, their playlist identity is the source-native key their projection
/// keeps in `file_path`: the server item id, carried source-prefixed.
pub fn local_picker_ref_for_track(track: &qbz_library::LocalTrack) -> String {
    match track.source.as_deref() {
        Some("plex") => format!("plex:{}", track.file_path),
        Some(source @ ("jellyfin" | "subsonic")) => format!("{source}:{}", track.file_path),
        _ => track.id.to_string(),
    }
}

pub(crate) fn total_duration_label(rows: &[LoadedRow]) -> String {
    let secs: u64 = rows
        .iter()
        .map(|r| match &r.item {
            RowItem::Qobuz(t) => t.duration as u64,
            RowItem::Cached { duration_secs, .. } => *duration_secs,
            RowItem::Local(t) | RowItem::Plex(t) => t.duration_secs,
            RowItem::LocalFile { .. } | RowItem::Unresolved { .. } => 0,
        })
        .sum();
    let mins = secs / 60;
    if mins >= 60 {
        qbz_i18n::t_args(
            "{} h {} min",
            &[&(mins / 60).to_string(), &(mins % 60).to_string()],
        )
    } else {
        qbz_i18n::t_args("{} min", &[&mins.to_string()])
    }
}

/// Build the display row + its queue track (when playable) for one resolved row.
pub(crate) fn row_to_display(item: &RowItem) -> (PlaylistTrackRow, Option<QueueTrack>) {
    match item {
        RowItem::Qobuz(track) => {
            let row = crate::playlist_qt::map_track(track);
            let queue = crate::playlist_qt::row_to_queue_public(&row);
            (row, Some(queue))
        }
        RowItem::Cached {
            track_id,
            title,
            artist,
            album,
            duration_secs,
            bit_depth,
            sample_rate,
            artwork_path,
        } => {
            let art = artwork_path.clone().unwrap_or_default();
            let row = PlaylistTrackRow {
                position: 0,
                featured: Vec::new(),
                id: track_id.to_string(),
                playlist_track_id: *track_id,
                title: title.clone(),
                artist: artist.clone(),
                album: album.clone(),
                duration: crate::playlist_qt::mmss((*duration_secs).min(u32::MAX as u64) as u32),
                duration_secs: *duration_secs,
                quality_tier: crate::playlist_qt::tier(*bit_depth).to_string(),
                quality_detail: crate::home_qt::quality_detail_from_parts(*bit_depth, *sample_rate),
                quality_label: crate::playlist_qt::quality_label(*bit_depth, *sample_rate),
                bit_depth: *bit_depth,
                sample_rate: *sample_rate,
                art_url: art.clone(),
                art_path: crate::artwork_qt::cached_path(&art),
                is_favorite: crate::fav_cache_qt::contains_track(*track_id),
                cache_status: 3,
                ..Default::default()
            };
            let queue = QueueTrack {
                id: *track_id,
                title: title.clone(),
                version: None,
                artist: artist.clone(),
                album: album.clone(),
                album_version: None,
                duration_secs: *duration_secs,
                artwork_url: artwork_path
                    .clone()
                    .map(|path| qbz_models::fs_url::file_url(&path)),
                hires: bit_depth.map(|depth| depth >= 24).unwrap_or(false),
                bit_depth: *bit_depth,
                sample_rate: *sample_rate,
                is_local: false,
                album_id: None,
                artist_id: None,
                streamable: true,
                source: Some("qobuz".to_string()),
                parental_warning: false,
                source_item_id_hint: None,
                context_kind: None,
                context_id: None,
                isrc: None,
                recording_mbid: None,
            };
            (row, Some(queue))
        }
        RowItem::Local(t) | RowItem::Plex(t) => {
            let queue = crate::local_playback::local_queue_track(t);
            let source = if matches!(item, RowItem::Plex(_)) {
                "plex"
            } else {
                // Remote sidecar rows resolve into RowItem::Local carrying
                // their own source; publishing it is what keeps
                // `local_picker_ref_for_row` and the queue typing honest.
                match t.source.as_deref() {
                    Some("jellyfin") => "jellyfin",
                    Some("subsonic") => "subsonic",
                    _ => "local",
                }
            };
            // The SAME artwork ref the queue/NPB build (`local_queue_track` →
            // `portable_artwork_ref`): a media-server row is namespaced
            // (`jellyfin:<id>` / `subsonic:<id>`) so `classify` can hand it to
            // the source registry to fetch; a local file / Plex thumb stays its
            // own on-disk path. The old code fed the RAW `artwork_path` — an
            // opaque token `cached_path` could not classify — which is why
            // media rows were art-less in the TrackRow while the queue and NPB,
            // going through this same ref, showed the cover fine.
            let art_ref =
                crate::local_rows::portable_artwork_ref(t, crate::local_rows::ArtworkScope::Track)
                    .unwrap_or_default();
            let row = PlaylistTrackRow {
                position: 0,
                featured: Vec::new(),
                id: queue.id.to_string(),
                playlist_track_id: queue.id,
                title: t.title.clone(),
                artist: t.artist.clone(),
                album: t.album.clone(),
                duration: crate::playlist_qt::mmss(t.duration_secs as u32),
                duration_secs: t.duration_secs,
                // Through the LOCAL row helpers, not the Qobuz ones: a local
                // track's `sample_rate` is in Hz (44100), the catalog's is in
                // kHz (44.1), and `tier_of` also reads the FORMAT — which is
                // how a lossless FLAC is told from an mp3 when the tags carry
                // no bit depth.
                quality_tier: crate::local_rows::tier_of(&t.format, t.bit_depth, t.sample_rate)
                    .to_string(),
                quality_detail: crate::local_rows::detail_of(&t.format, t.bit_depth, t.sample_rate),
                // The raw numbers, normalized to kHz exactly like
                // `local_playback::local_queue_track` (:155-159) does — a local
                // row reports Hz (44100), the catalog reports kHz (44.1), and
                // `quality_state::rate_to_hz` must not multiply a kHz value
                // twice. Kept in sync with the row's own `quality_detail`
                // above so nothing downstream has to re-parse the string.
                bit_depth: t.bit_depth,
                sample_rate: Some(if t.sample_rate >= 1000.0 {
                    t.sample_rate / 1000.0
                } else {
                    t.sample_rate
                }),
                art_url: art_ref.clone(),
                // `TrackRow.qml` renders `artPath`, never `artUrl`: a disk hit
                // (local file / Plex thumb) resolves inline here; a media cover
                // whose bytes are not cached yet fills in on the load's
                // artwork-warm republish (`cached_path` re-run after
                // `download_missing`). Without a resolvable ref the rows
                // rendered art-less while the sidebar collage did not.
                art_path: crate::artwork_qt::cached_path(&art_ref),
                source: source.to_string(),
                ..Default::default()
            };
            (row, Some(queue))
        }
        RowItem::LocalFile { path } => {
            // No metadata anywhere, but the file is on disk: show the filename
            // so the row is identifiable, and mark it unplayable until the
            // library index knows it again.
            let name = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            (
                PlaylistTrackRow {
                    position: 0,
                    featured: Vec::new(),
                    id: path.clone(),
                    title: name,
                    source: "local".to_string(),
                    unavailable: true,
                    unavailable_ref: path.clone(),
                    ..Default::default()
                },
                None,
            )
        }
        RowItem::Unresolved { kind, reference } => (
            PlaylistTrackRow {
                position: 0,
                featured: Vec::new(),
                id: format!("{kind}:{reference}"),
                title: qbz_i18n::t("Unavailable track"),
                source: (*kind).to_string(),
                unavailable: true,
                unavailable_ref: reference.clone(),
                ..Default::default()
            },
            None,
        ),
    }
}

fn build_row_models(
    rows: &[LoadedRow],
) -> (Vec<QueueTrack>, Vec<PlaylistTrackRow>, HashMap<String, i32>) {
    let mut display = Vec::with_capacity(rows.len());
    let mut queue = Vec::new();
    let mut positions = HashMap::new();
    for row in rows {
        let (item, track) = row_to_display(&row.item);
        positions.insert(item.id.clone(), row.position);
        if let Some(track) = track {
            queue.push(track);
        }
        display.push(item);
    }
    (queue, display, positions)
}

fn merge_offline_rows(
    playable_ids: &[u64],
    cached: &HashMap<u64, RowItem>,
    mut sidecars: Vec<LoadedRow>,
) -> Vec<LoadedRow> {
    let mut rows: Vec<LoadedRow> = playable_ids
        .iter()
        .enumerate()
        .filter_map(|(position, track_id)| {
            cached.get(track_id).cloned().map(|item| LoadedRow {
                position: position as i32,
                item,
            })
        })
        .collect();
    // Snapshot positions and sidecar positions are different coordinate
    // systems after filtering: keep each block's stable order rather than
    // interleaving absolute sidecar slots through a cached-only subset.
    sidecars.sort_by_key(|row| row.position);
    rows.extend(sidecars);
    rows
}

fn offline_cover_urls(rows: &[LoadedRow]) -> Vec<String> {
    let mut covers = Vec::new();
    for row in rows {
        let artwork = match &row.item {
            RowItem::Cached { artwork_path, .. } => artwork_path.as_deref(),
            RowItem::Local(track) | RowItem::Plex(track) => track.artwork_path.as_deref(),
            RowItem::Qobuz(_) | RowItem::LocalFile { .. } | RowItem::Unresolved { .. } => None,
        };
        let Some(artwork) = artwork.filter(|value| !value.is_empty()) else {
            continue;
        };
        if !covers.iter().any(|cover| cover == artwork) {
            covers.push(artwork.to_string());
        }
        if covers.len() == 4 {
            break;
        }
    }
    covers
}

/// Load + resolve a local playlist and publish it through the SHARED playlist
/// view. Qobuz rows resolve in one batch fetch with a Ready offline-cache
/// fallback; local rows come from library.db by path and Plex rows from the
/// Plex cache in one bulk lookup.
///
/// Runs off the Qt thread. Returns false when the playlist does not exist.
pub async fn load(runtime: &Runtime, playlist_id: &str) -> bool {
    let id = playlist_id.to_string();
    let Ok((header, tracks)) = tokio::task::spawn_blocking({
        let id = id.clone();
        move || (get_blocking(&id), get_tracks_blocking(&id))
    })
    .await
    else {
        return false;
    };
    let Some(header) = header else {
        log::warn!("[qbz-qt] local playlist {id}: not found");
        return false;
    };

    // Qobuz rows: ONE batch fetch online, then the persistent Ready-cache
    // metadata for misses (or for the whole set offline). Rows absent from
    // both sources hide; a local-only playlist remains unaffected.
    let offline = crate::offline_fwd::engine().is_offline();
    let qobuz_ids: Vec<u64> = tracks.iter().filter_map(|t| t.qobuz_track_id).collect();
    let mut fetched: HashMap<u64, qbz_models::Track> = HashMap::new();
    if !offline && !qobuz_ids.is_empty() {
        match runtime.core().get_tracks_batch(&qobuz_ids).await {
            Ok(list) => {
                for t in list {
                    fetched.insert(t.id, t);
                }
            }
            Err(e) => log::warn!("[qbz-qt] local playlist {id}: qobuz batch failed: {e}"),
        }
    }
    let missing_qobuz_ids: Vec<u64> = qobuz_ids
        .iter()
        .copied()
        .filter(|track_id| !fetched.contains_key(track_id))
        .collect();
    let cached = ready_cached_rows(&missing_qobuz_ids).await;

    // Plex rows: ONE bulk cache lookup by rating key.
    let plex_keys: Vec<String> = tracks
        .iter()
        .filter_map(|t| t.plex_key.clone())
        .filter(|k| !k.is_empty())
        .collect();
    let plex_resolved: HashMap<String, qbz_library::LocalTrack> = if plex_keys.is_empty() {
        HashMap::new()
    } else {
        tokio::task::spawn_blocking(move || {
            match qbz_plex::plex_cache_get_cached_tracks_by_keys(&plex_keys) {
                Ok(rows) => rows
                    .into_iter()
                    .map(crate::local_plex::map_cached_to_local_track)
                    .map(|t| (t.file_path.clone(), t))
                    .collect(),
                Err(e) => {
                    log::warn!("[qbz-qt] local playlist: plex cache resolve failed: {e}");
                    HashMap::new()
                }
            }
        })
        .await
        .unwrap_or_default()
    };

    // Local rows: resolve by path, and stat the misses on the same worker so
    // an unindexed-but-present file still renders.
    let local_paths: Vec<String> = tracks.iter().filter_map(|t| t.local_path.clone()).collect();
    let (locals, on_disk): (
        HashMap<String, qbz_library::LocalTrack>,
        std::collections::HashSet<String>,
    ) = if local_paths.is_empty() {
        Default::default()
    } else {
        tokio::task::spawn_blocking(move || {
            let resolved = with_db(false, |db| {
                let mut out = HashMap::new();
                for path in &local_paths {
                    if let Some(track) = db.get_track_by_path(path)? {
                        out.insert(path.clone(), track);
                    }
                }
                Ok(out)
            })
            .unwrap_or_default();
            let on_disk: std::collections::HashSet<String> = local_paths
                .iter()
                .filter(|p| !resolved.contains_key(*p))
                .filter(|p| std::path::Path::new(p.as_str()).exists())
                .cloned()
                .collect();
            (resolved, on_disk)
        })
        .await
        .unwrap_or_default()
    };

    // Jellyfin/Subsonic rows: ONE bulk media-cache lookup by item id (their
    // item id rides the local_path column, source-qualified).
    let remote_refs: Vec<(String, String, i32)> = tracks
        .iter()
        .filter_map(|t| {
            let source = match t.source {
                repo::LocalPlaylistTrackSource::Jellyfin => "jellyfin",
                repo::LocalPlaylistTrackSource::Subsonic => "subsonic",
                _ => return None,
            };
            t.local_path
                .clone()
                .map(|item| (source.to_string(), item, 0))
        })
        .collect();
    let remote_resolved: HashMap<(String, String), qbz_library::LocalTrack> =
        if remote_refs.is_empty() {
            HashMap::new()
        } else {
            tokio::task::spawn_blocking(move || {
                crate::media_servers_qt::tracks_by_item_ids_blocking(&remote_refs)
            })
            .await
            .unwrap_or_default()
        };

    let mut rows: Vec<LoadedRow> = Vec::new();
    let (mut hidden, mut missing_files, mut unresolved) = (0usize, 0usize, 0usize);
    for t in tracks {
        let item = match t.source {
            repo::LocalPlaylistTrackSource::Qobuz => {
                let Some(tid) = t.qobuz_track_id else {
                    hidden += 1;
                    continue;
                };
                if tid >= crate::local_plex::PLEX_TRACK_ID_FLOOR {
                    unresolved += 1;
                    log::warn!(
                        "[qbz-qt] local playlist {id}: qobuz ref {tid} is outside the catalog \
                         range (legacy mis-typed row) — rendered as unavailable"
                    );
                    RowItem::Unresolved {
                        kind: "qobuz",
                        reference: tid.to_string(),
                    }
                } else if let Some(track) = fetched.remove(&tid) {
                    RowItem::Qobuz(Box::new(track))
                } else if let Some(track) = cached.get(&tid).cloned() {
                    track
                } else {
                    hidden += 1;
                    continue;
                }
            }
            repo::LocalPlaylistTrackSource::Local => match t.local_path.as_ref() {
                Some(p) => {
                    if let Some(track) = locals.get(p) {
                        RowItem::Local(Box::new(track.clone()))
                    } else if on_disk.contains(p) {
                        RowItem::LocalFile { path: p.clone() }
                    } else {
                        missing_files += 1;
                        continue;
                    }
                }
                None => {
                    hidden += 1;
                    continue;
                }
            },
            repo::LocalPlaylistTrackSource::Plex => {
                let key = t.plex_key.clone().unwrap_or_default();
                match plex_resolved.get(&key) {
                    Some(track) => RowItem::Plex(Box::new(track.clone())),
                    None => {
                        unresolved += 1;
                        log::warn!(
                            "[qbz-qt] local playlist {id}: plex key {key:?} not in the Plex \
                             cache — rendered as unavailable"
                        );
                        RowItem::Unresolved {
                            kind: "plex",
                            reference: key,
                        }
                    }
                }
            }
            repo::LocalPlaylistTrackSource::Jellyfin | repo::LocalPlaylistTrackSource::Subsonic => {
                let source = t.source.as_str();
                let item_id = t.local_path.clone().unwrap_or_default();
                match remote_resolved.get(&(source.to_string(), item_id.clone())) {
                    Some(track) => RowItem::Local(Box::new(track.clone())),
                    None => {
                        unresolved += 1;
                        log::warn!(
                            "[qbz-qt] local playlist {id}: {source} item {item_id:?} not in \
                             the media cache — rendered as unavailable"
                        );
                        RowItem::Unresolved {
                            kind: if source == "jellyfin" {
                                "jellyfin"
                            } else {
                                "subsonic"
                            },
                            reference: item_id,
                        }
                    }
                }
            }
        };
        rows.push(LoadedRow {
            position: t.position,
            item,
        });
    }
    if hidden > 0 {
        log::info!("[qbz-qt] local playlist {id}: {hidden} row(s) unavailable, hidden");
    }
    if missing_files > 0 {
        log::info!(
            "[qbz-qt] local playlist {id}: {missing_files} local file row(s) missing on disk, hidden"
        );
    }
    if unresolved > 0 {
        log::info!(
            "[qbz-qt] local playlist {id}: {unresolved} row(s) with unresolvable refs, rendered as unavailable"
        );
    }

    // Build the display rows + the playable snapshot in ONE pass, so a row's
    // display id and its queue entry can never disagree.
    let (queue, display, positions) = build_row_models(&rows);

    let covers = tokio::task::spawn_blocking({
        let id = id.clone();
        move || resolve_cover_urls_blocking(&id, 4)
    })
    .await
    .unwrap_or_default();

    // Custom cover — read the SAME store the header menu writes.
    //
    // This doc used to read only `library.db`'s `custom_artwork_path`, which
    // nothing in Qt writes any more: the menu on this very page persists to
    // `custom_playlist_covers.json` through `cover_artwork_qt` (that split is
    // deliberate — a `playlists` key inside the shared `custom_artwork.json`
    // would be dropped by the other build's next write). So on a LOCAL
    // playlist a cover the user just picked appeared in the sidebar and never
    // in this header, and `has_custom_cover` stayed false, which meant the
    // menu only ever offered "Add cover" — "Remove" was unreachable.
    //
    // The DB column survives as a READ fallback so a cover stored there by an
    // earlier build is not silently lost; `clear_custom_artwork_blocking` is
    // what lets Remove clear one of those.
    let custom_cover = crate::cover_artwork_qt::playlist_cover(&id)
        .or_else(|| header.custom_artwork_path.clone().filter(|p| !p.is_empty()));

    let doc = PlaylistDoc {
        id: header.id.clone(),
        name: header.name.clone(),
        description: header.description.clone().unwrap_or_default(),
        cover_path: custom_cover.clone().unwrap_or_default(),
        has_custom_cover: custom_cover.is_some(),
        covers,
        track_count: display.len() as i32,
        total_duration: total_duration_label(&rows),
        tracks: display,
        // A local playlist is always the user's own; there is nobody to
        // follow it from and nothing to copy it into.
        is_owner: true,
        is_local_playlist: true,
        offline_only: header.offline_only,
        ..Default::default()
    };

    if let Ok(mut cur) = CURRENT_QUEUE.lock() {
        *cur = queue;
    }
    if let Ok(mut meta) = CURRENT_META.lock() {
        *meta = Some((header.id.clone(), header.offline_only));
    }
    if let Ok(mut pos) = ROW_POSITIONS.lock() {
        *pos = positions;
    }
    // The LOCAL half of the "Recently Played Playlists" meta. Same contract as
    // the Qobuz loader: metadata only, the play event comes from the
    // track-start edge, and the rail's JOIN keeps a merely-browsed playlist
    // off it. `source: "local"` records where the PLAYLIST lives — its rows
    // can still be Qobuz tracks.
    qbz_app::settings::playlist_play_history::record_playlist_meta(
        qbz_app::settings::playlist_play_history::PlaylistPlayMeta {
            playlist_id: &doc.id,
            title: &doc.name,
            owner: "",
            owner_id: "",
            // A local playlist never has a Qobuz graphic, so unless the user
            // set a cover it is the MOSAIC that carries the card — the same
            // shape this page's own header takes.
            artwork_url: &doc.cover_path,
            own_image: doc.has_custom_cover,
            covers: &doc.covers,
            track_count: doc.track_count.max(0) as u32,
            source: "local",
        },
    );
    // Media-server sidecar covers (jellyfin/subsonic) are namespaced refs
    // whose bytes may not be cached yet. Unlike the Qobuz loader this path has
    // no inline artwork-warm, so a row keyed on such a ref rendered art-less
    // until the track was played (the queue/NPB fetch on their own). Collect
    // the unresolved refs BEFORE the doc is adopted, then fetch + republish;
    // local files and Plex thumbs resolved inline in `row_to_display` and
    // never reach this list, so a pure-local playlist spawns nothing.
    let missing_art: Vec<String> = doc
        .tracks
        .iter()
        .filter(|r| r.art_path.is_empty() && !r.art_url.is_empty())
        .map(|r| r.art_url.clone())
        .collect();
    crate::playlist_qt::adopt_doc(doc);
    if !missing_art.is_empty() {
        crate::spawn(async move {
            crate::artwork_qt::download_missing(missing_art).await;
            crate::playlist_qt::rewarm_row_art();
        });
    }
    true
}

/// Open a numeric Qobuz playlist without touching the network.
///
/// The Qobuz block is the persisted membership intersected with Ready
/// downloads. Local sidecars always remain visible; Plex sidecars remain only
/// while raw connectivity is Up (manual offline or a logged-out session with
/// a reachable LAN). Uncached Qobuz members are hidden, which keeps every
/// published row playable or honestly marked unavailable.
pub async fn load_qobuz_offline(playlist_id: u64) -> bool {
    let plex_allowed = crate::offline_fwd::engine().status().connectivity
        == qbz_app::offline_mode::Connectivity::Up;
    let loaded = tokio::task::spawn_blocking(move || {
        let headers = crate::playlist_snapshot_qt::headers_blocking();
        let qobuz_count = crate::sidebar_qt::playlist_track_count(playlist_id)
            .or_else(|| headers.get(&playlist_id).and_then(|(_, count)| *count))
            .unwrap_or(0);
        let sidecars = read_sidecar_rows_blocking(playlist_id, qobuz_count, plex_allowed);
        let playable_ids = crate::playlist_snapshot_qt::playable_track_ids_blocking(playlist_id);
        let name = crate::sidebar_qt::playlist_name(playlist_id)
            .or_else(|| headers.get(&playlist_id).map(|(name, _)| name.clone()))
            .or_else(|| crate::playlist_snapshot_qt::name_blocking(playlist_id))
            .unwrap_or_else(|| qbz_i18n::t("Playlist"));
        (sidecars, playable_ids, name)
    })
    .await;
    let Ok((sidecars, playable_ids, name)) = loaded else {
        return false;
    };

    let cached = ready_cached_rows(&playable_ids).await;
    let rows = merge_offline_rows(&playable_ids, &cached, sidecars);
    let hidden_snapshot_rows = playable_ids.len().saturating_sub(
        rows.iter()
            .filter(|row| matches!(&row.item, RowItem::Cached { .. }))
            .count(),
    );
    let covers = offline_cover_urls(&rows);
    let (queue, display, positions) = build_row_models(&rows);
    let custom_cover = crate::cover_artwork_qt::playlist_cover(&playlist_id.to_string())
        .filter(|path| std::path::Path::new(path).is_file());
    let doc = PlaylistDoc {
        id: playlist_id.to_string(),
        name,
        owner: qbz_i18n::t("Available tracks only — offline"),
        cover_url: custom_cover.clone().unwrap_or_default(),
        cover_path: custom_cover.clone().unwrap_or_default(),
        has_custom_cover: custom_cover.is_some(),
        covers,
        track_count: display.len() as i32,
        total_duration: total_duration_label(&rows),
        tracks: display,
        // The page is intentionally read-only while offline. It still uses
        // the mixed/source-aware queue even when there are no sidecar rows.
        is_owner: false,
        is_mixed: true,
        ..Default::default()
    };

    set_open_mixed_snapshot(&doc.id, queue, positions);
    crate::playlist_qt::mark_mixed();
    log::info!(
        "[qbz-qt] offline mixed playlist {}: {} cached Qobuz, {} sidecar, {} stale cache entries hidden",
        playlist_id,
        rows.iter()
            .filter(|row| matches!(&row.item, RowItem::Cached { .. }))
            .count(),
        rows.iter()
            .filter(|row| !matches!(&row.item, RowItem::Cached { .. }))
            .count(),
        hidden_snapshot_rows,
    );
    crate::playlist_qt::adopt_doc(doc);
    true
}

// ──────────────────────────── playback ────────────────────────────

/// Play the open local detail from `start_row_id` ("" = from the top).
///
/// D8: an offline-only playlist installs the queue together with its
/// offline-only stamp, which keeps ANY of its tracks from being pushed to
/// Qobuz Connect. Every non-offline-only replacement clears the stamp so it
/// cannot leak into the next thing the user plays.
pub async fn play(runtime: &Runtime, start_row_id: &str) {
    play_in(runtime, start_row_id, false).await
}

/// Header Shuffle for a LOCAL playlist: the same path, with the list mixed and
/// the anchor dropped.
///
/// Raising the shuffle MODE and playing from the top is NOT a shuffle — the
/// mode only randomises what comes NEXT, so the first track was the playlist's
/// #1 every time. Owner ruling 2026-08-01: every shuffle must be genuinely
/// random. Same shape as `playlist_qt::play_shuffled` and
/// `playback_qt::play_track_list_in`.
pub async fn play_shuffled(runtime: &Runtime) {
    play_in(runtime, "", true).await
}

/// Header context-menu queueing for a local or mixed OPEN playlist.
///
/// These details already own a source-aware resolved snapshot. Re-fetching by
/// numeric playlist id would lose local/Plex sidecars and cannot represent a
/// `local:<uuid>` at all, so all three insertion modes consume that snapshot
/// just like header Play and Shuffle do.
pub async fn enqueue_all(runtime: &Runtime, mode: &str) -> Result<(), String> {
    let (queue, playlist_id) = {
        let queue = CURRENT_QUEUE
            .lock()
            .map_err(|_| "open playlist queue is unavailable".to_string())?
            .clone();
        let playlist_id = CURRENT_META
            .lock()
            .map_err(|_| "open playlist metadata is unavailable".to_string())?
            .as_ref()
            .map(|(id, _)| id.clone())
            .unwrap_or_default();
        (queue, playlist_id)
    };
    let queue = crate::playback_qt::stamped(
        queue,
        crate::playback_qt::PlayContext::playlist(&playlist_id),
    );
    if queue.is_empty() {
        return Ok(());
    }
    crate::playback_qt::enqueue_track_list_mode(runtime, queue, mode).await
}

async fn play_in(runtime: &Runtime, start_row_id: &str, shuffle: bool) {
    let (queue, offline_only, playlist_id) = {
        let Ok(q) = CURRENT_QUEUE.lock() else { return };
        let meta = CURRENT_META.lock().ok().and_then(|m| m.clone());
        let offline_only = meta.as_ref().map(|(_, o)| *o).unwrap_or(false);
        let playlist_id = meta.map(|(id, _)| id).unwrap_or_default();
        (q.clone(), offline_only, playlist_id)
    };
    if queue.is_empty() {
        return;
    }
    let mut queue = queue;
    let start = if shuffle {
        crate::playback_qt::xorshift_shuffle(&mut queue);
        0
    } else if start_row_id.is_empty() {
        0
    } else {
        queue
            .iter()
            .position(|t| t.id.to_string() == start_row_id)
            .unwrap_or(0)
    };
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    // The "playing from" origin is ("playlist", <local:uuid>) — the reference
    // stamps it on BOTH local play paths (`local_playlist.rs:1578` play_all /
    // Shuffle and `:1599` play_from_visible), with the explicit note that "the
    // now-playing context stays ('playlist', id)" for a mixed detail because
    // anything Qobuz-bound re-resolves membership and excludes the sidecar rows
    // by construction (playback.rs:3478-3488).
    //
    // Passing `None` here was the port's gap: `set_queue_stamped` then falls to
    // `derive_context`, which returns None for a many-album playlist, so
    // `refresh_now_playing` used its per-track ALBUM fallback and the song card
    // drew the album glyph pointing at a local `album_group_key` instead of the
    // playlist. It also made the row click REGRESS once main.rs started routing
    // it here — `playlist_qt::play_track` had been stamping `open_context()`.
    // F1: the anchor comes back from the seam. A local playlist's rows are
    // MIXED — it can hold real Qobuz tracks beside local files — so the filter
    // genuinely fires here, and reading the id off the pre-filter list would
    // replay one the core just dropped.
    let Some(anchor) = crate::playback_qt::set_queue_stamped_with_offline_only(
        runtime,
        queue,
        Some(start),
        crate::playback_qt::PlayContext::playlist(&playlist_id),
        offline_only,
    )
    .await
    else {
        log::info!("[qbz-qt] local playlist play: every track was filtered, queue untouched");
        return;
    };
    crate::playback_qt::play_queue_track_public(runtime, anchor.track_id).await;
}

/// Remove the open detail's row by display id, then reload.
///
/// The row menu's "Remove from playlist" on a LOCAL detail lands here through
/// `crate::playlist_remove_track` — the Qobuz arm (`playlist_qt::remove_track`)
/// cannot serve it: it parses the open document's id as a `u64` and a
/// `local:<uuid>` never parses, so it returned without doing anything (the
/// renders-and-no-ops defect the owner reported).
///
/// The reload is what makes the row disappear: `load` rebuilds the document
/// from the repo and republishes it through `playlist_qt::adopt_doc`, so the
/// view settles on the same path the initial open uses instead of on an
/// optimistic patch that would then have to be reconciled. It is OFFLINE-SAFE:
/// the repo write is a local SQLite write and the reload degrades exactly as
/// opening the playlist offline already does (Qobuz-sourced rows hide, local
/// and Plex rows stay).
pub async fn remove_row(runtime: &Runtime, row_id: &str) {
    let Some((playlist_id, _)) = CURRENT_META.lock().ok().and_then(|m| m.clone()) else {
        return;
    };
    let Some(position) = ROW_POSITIONS
        .lock()
        .ok()
        .and_then(|p| p.get(row_id).copied())
    else {
        log::warn!("[qbz-qt] local playlist remove: unknown row {row_id}");
        return;
    };
    let pid = playlist_id.clone();
    let _ = tokio::task::spawn_blocking(move || {
        with_db(true, |db| {
            Ok(db.with_connection(|conn| repo::remove_track(conn, &pid, position)))
        })
    })
    .await;
    load(runtime, &playlist_id).await;
    // The sidebar row's cover collage is built from this playlist's MEMBER
    // covers, so dropping a row can change it. `reload_sidebar_including_local`
    // is the offline-safe verb — `reload_sidebar()` early-returns offline and
    // would leave the collage stale for exactly the users local playlists
    // exist for.
    crate::reload_sidebar_including_local();
}

/// Reorder the open detail: move the row at `from` to `to` (repo positions),
/// then reload.
///
/// `repo::reorder` is remove-then-insert over the stored `position` column, so
/// BOTH endpoints must name rows that exist; it no-ops otherwise. The two
/// public entry points below ([`move_row`], [`reorder_row`]) are what turn the
/// VISIBLE order the view speaks in into these positions.
pub async fn reorder(runtime: &Runtime, from: i32, to: i32) {
    let Some((playlist_id, _)) = CURRENT_META.lock().ok().and_then(|m| m.clone()) else {
        return;
    };
    if from == to {
        return;
    }
    let pid = playlist_id.clone();
    let _ = tokio::task::spawn_blocking(move || {
        with_db(true, |db| {
            Ok(db.with_connection(|conn| repo::reorder(conn, &pid, from, to)))
        })
    })
    .await;
    load(runtime, &playlist_id).await;
}

/// The repo positions of two rows named by display id, in one lock.
///
/// Hidden rows (an unresolvable Qobuz ref while offline, a local file that is
/// gone) keep their own `position` in the DB but never reach the document, so
/// the visible order has GAPS in position space. Resolving both endpoints
/// through the map instead of doing index arithmetic is what makes the move
/// land exactly at the anchor's slot across those gaps — the reference states
/// the same rule (`local_playlist.rs:1845-1848`).
fn positions_of(a: &str, b: &str) -> Option<(i32, i32)> {
    let map = ROW_POSITIONS.lock().ok()?;
    Some((map.get(a).copied()?, map.get(b).copied()?))
}

/// Arrow reorder — move the row `row_id` one slot up (`delta < 0`) or down
/// (`delta > 0`) in the open LOCAL detail's natural (repo) order.
///
/// The local branch of the detail view's reorder chevrons, the twin of
/// `playlist_qt::move_row` (which rebuilds the Qobuz custom-order sidecar).
/// There is no sidecar here: the repo `position` IS the order, which is also
/// why the reference hides the "Custom" sort option on a local playlist (B2).
pub async fn move_row(runtime: &Runtime, row_id: &str, delta: i32) {
    if delta == 0 {
        return;
    }
    let ids = crate::playlist_qt::row_ids();
    let Some(idx) = ids.iter().position(|id| id.as_str() == row_id) else {
        log::warn!("[qbz-qt] local playlist move: row {row_id} is not on the open page");
        return;
    };
    let neighbour = idx as i32 + delta.signum();
    if neighbour < 0 || neighbour as usize >= ids.len() {
        return; // already first / last
    }
    let Some((from, to)) = positions_of(row_id, &ids[neighbour as usize]) else {
        return;
    };
    reorder(runtime, from, to).await;
}

/// Drag reorder — move the visible row `from_row` to insertion slot `to_slot`
/// (0..=N, the gap the pointer was released over).
///
/// The ANCHOR row is what turns a gap into a position: moving DOWN the dragged
/// row lands right after the row above the gap (`to_slot - 1`), moving UP it
/// lands right before the row at the gap (`to_slot`). Same resolution as the
/// reference (`local_playlist.rs:1816-1832`).
pub async fn reorder_row(runtime: &Runtime, from_row: usize, to_slot: usize) {
    // The two slots that drop back onto the same gap. The view already skips
    // them; the guard is kept because this is a public entry point.
    if to_slot == from_row || to_slot == from_row + 1 {
        return;
    }
    let ids = crate::playlist_qt::row_ids();
    if from_row >= ids.len() || to_slot > ids.len() {
        return;
    }
    let anchor = if to_slot > from_row {
        to_slot - 1
    } else {
        to_slot
    };
    if anchor == from_row {
        return;
    }
    let Some((from, to)) = positions_of(&ids[from_row], &ids[anchor]) else {
        return;
    };
    reorder(runtime, from, to).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached(track_id: u64) -> RowItem {
        cached_with_art(track_id, None)
    }

    fn cached_with_art(track_id: u64, artwork_path: Option<&str>) -> RowItem {
        RowItem::Cached {
            track_id,
            title: format!("Track {track_id}"),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_secs: 181,
            bit_depth: Some(24),
            sample_rate: Some(96.0),
            artwork_path: artwork_path.map(str::to_string),
        }
    }

    fn row_key(row: &LoadedRow) -> String {
        match &row.item {
            RowItem::Cached { track_id, .. } => format!("cached:{track_id}"),
            RowItem::LocalFile { path } => format!("local:{path}"),
            RowItem::Unresolved { kind, reference } => format!("{kind}:{reference}"),
            _ => "other".into(),
        }
    }

    #[test]
    fn offline_merge_keeps_snapshot_duplicates_then_sorted_sidecars() {
        let cached_rows = HashMap::from([(7, cached(7)), (9, cached(9))]);
        let sidecars = vec![
            LoadedRow {
                position: 40,
                item: RowItem::Unresolved {
                    kind: "plex",
                    reference: "later".into(),
                },
            },
            LoadedRow {
                position: 20,
                item: RowItem::LocalFile {
                    path: "/music/first.flac".into(),
                },
            },
        ];

        let rows = merge_offline_rows(&[7, 3, 9, 7], &cached_rows, sidecars);
        assert_eq!(
            rows.iter().map(row_key).collect::<Vec<_>>(),
            vec![
                "cached:7",
                "cached:9",
                "cached:7",
                "local:/music/first.flac",
                "plex:later",
            ]
        );
    }

    #[test]
    fn cached_row_is_catalog_typed_but_ready_offline() {
        let (row, queue) = row_to_display(&cached(77));
        let queue = queue.expect("ready cache row must be playable");
        assert_eq!(row.id, "77");
        assert!(row.source.is_empty());
        assert_eq!(row.cache_status, 3);
        assert_eq!(row.quality_tier, "hires");
        assert_eq!(queue.id, 77);
        assert_eq!(queue.source.as_deref(), Some("qobuz"));
        assert!(!queue.is_local);
        assert!(queue.streamable);
    }

    #[test]
    fn offline_cover_list_is_stable_deduplicated_and_capped() {
        let rows: Vec<LoadedRow> = ["a", "b", "a", "c", "d", "e"]
            .into_iter()
            .enumerate()
            .map(|(position, cover)| LoadedRow {
                position: position as i32,
                item: cached_with_art(position as u64, Some(cover)),
            })
            .collect();
        assert_eq!(offline_cover_urls(&rows), vec!["a", "b", "c", "d"]);
    }
}
