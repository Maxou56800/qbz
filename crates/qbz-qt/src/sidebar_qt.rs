//! Sidebar playlists + folders controller — Slint-free port of
//! `crates/qbz/src/sidebar.rs` (load / sort / search / expand / rebuild).
//!
//! Model: Qobuz playlists (`get_user_playlists`) + folder metadata from the
//! per-user library.db (`qbz-library` queries — same ones `folders.rs`
//! wraps), session-only expand state (Tauri parity — not persisted),
//! 5-option sort with direction toggle (#657), recursive name search.
//! Entries publish as ONE JSON document (`sidebarJson`).
//!
//! Local playlists, offline synthesis, hidden filtering, folder mutations,
//! row context menus, the mini-state folder flyout and playlist navigation
//! are all live. This module owns the data/tree side; the interaction surfaces
//! live in `qml/shell/Sidebar*.qml` and the folder controllers.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

use qbz_app::settings::pinned_items::{PinnedItem, PinnedItemsService, DB_FILE_NAME};
use qbz_app::shell::AppRuntime;
use qbz_app::user_data::UserDataPaths;
use qbz_core::LoggingAdapter;
use serde::Serialize;

// ---------------------------------------------------------------------------
// Pinned items store (PinnedItemsService, ADR-006 backend) — same lifecycle
// as the local-favorites store.
// ---------------------------------------------------------------------------

static PINNED: LazyLock<Mutex<Option<PinnedItemsService>>> = LazyLock::new(|| Mutex::new(None));
/// The per-user base dir the pinned store was bound to (also where the
/// discover-prefs DB lives) — stashed so Home can read section prefs.
static USER_DIR: LazyLock<Mutex<Option<std::path::PathBuf>>> = LazyLock::new(|| Mutex::new(None));

/// Bind `<dir>/pinned_items.db` on every session activation (mirrors
/// `pinned::init_for_user`; fail-open).
pub fn init_pinned(base_dir: &Path) {
    if let Ok(mut dir) = USER_DIR.lock() {
        *dir = Some(base_dir.to_path_buf());
    }
    // Never retain the previous profile's connection if the replacement
    // cannot be opened.
    if let Ok(mut service) = PINNED.lock() {
        *service = None;
    }
    let path = base_dir.join(DB_FILE_NAME);
    match PinnedItemsService::new(&path) {
        Ok(service) => {
            if let Ok(mut guard) = PINNED.lock() {
                *guard = Some(service);
            }
            log::info!("[qbz-qt] pinned items store opened");
        }
        Err(e) => log::error!("[qbz-qt] pinned items store open failed: {e}"),
    }
}

pub fn is_pinned(kind: &str, id: &str) -> bool {
    PINNED
        .lock()
        .ok()
        .and_then(|service| service.as_ref().map(|s| s.is_pinned(kind, id)))
        .unwrap_or(false)
}

/// All pinned items, most-recent first (PinnedItemsService::list) — feeds
/// the Home "Pinned" rail.
pub fn list_pinned() -> Vec<PinnedItem> {
    PINNED
        .lock()
        .ok()
        .and_then(|service| service.as_ref().and_then(|s| s.list().ok()))
        .unwrap_or_default()
}

/// The per-user base dir (None before any session activation).
pub fn user_dir() -> Option<std::path::PathBuf> {
    USER_DIR.lock().ok().and_then(|dir| dir.clone())
}

/// Toggle the pin state of an album/artist/playlist. Returns the new
/// state, or None with no store bound / on error.
pub fn toggle_pin(
    kind: &str,
    id: &str,
    title: &str,
    subtitle: &str,
    artwork_url: &str,
) -> Option<bool> {
    let service = PINNED.lock().ok()?;
    let service = service.as_ref()?;
    if service.is_pinned(kind, id) {
        service.unpin(kind, id).ok()?;
        Some(false)
    } else {
        service
            .pin(&PinnedItem {
                kind: kind.to_string(),
                id: id.to_string(),
                title: title.to_string(),
                subtitle: subtitle.to_string(),
                artwork_url: artwork_url.to_string(),
                pinned_at: 0,
            })
            .ok()?;
        Some(true)
    }
}

// ---------------------------------------------------------------------------
// Sidebar model
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize)]
pub struct SidebarEntry {
    pub kind: String, // "folder" | "playlist"
    pub id: String,
    pub name: String,
    pub expanded: bool,
    pub count: i32,
    pub indent: bool,
    #[serde(rename = "folderId")]
    pub folder_id: String,
    /// Up to 4 de-duplicated cover urls for the 2x2 micro-collage.
    pub covers: Vec<String>,
    /// A first-class LOCAL playlist (`local:<uuid>`), not a Qobuz one. The row
    /// carries a hard-drive mark and routes to the local detail; its id is a
    /// string, so nothing may parse it as a catalog number.
    #[serde(
        default,
        rename = "isLocal",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub is_local: bool,
}

/// One LOCAL playlist listed in the sidebar (library.db, ids `local:<uuid>`).
///
/// Present online AND offline — that is the whole point of the feature for
/// people running QBZ without Qobuz. Folder membership rides
/// `local_playlists.folder_id`, which points at the SAME folders the Qobuz
/// playlists use, so one folder holds both kinds.
#[derive(Clone)]
struct SidebarLocal {
    id: String,
    name: String,
    folder_id: Option<String>,
    covers: Vec<String>,
}

#[derive(Clone)]
struct SidebarPlaylist {
    id: u64,
    name: String,
    tracks_count: u32,
    cover_urls: Vec<String>,
    position: i32,
}

#[derive(Default, Clone)]
struct SidebarData {
    playlists: Vec<SidebarPlaylist>,
    /// (id, name), hidden folders already excluded.
    folders: Vec<(String, String)>,
    /// playlist id -> folder id.
    folder_map: HashMap<u64, String>,
    hidden_playlists: HashSet<u64>,
    /// First-class local playlists, listed alongside the Qobuz set.
    locals: Vec<SidebarLocal>,
    // Read-after-write protection while the remote list catches up. Kept in
    // the same mutex as the rows so an already running load cannot drop them.
    recent_qobuz: HashMap<u64, std::time::Instant>,
}

static CACHE: Mutex<Option<SidebarData>> = Mutex::new(None);
static INSERT_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn insertion_revision() -> u64 {
    INSERT_REVISION.load(std::sync::atomic::Ordering::SeqCst)
}

fn preserve_recent_entries(
    previous: &SidebarData,
    next: &mut SidebarData,
    now: std::time::Instant,
    online: bool,
) {
    let recent =
        |at: &std::time::Instant| now.duration_since(*at) < std::time::Duration::from_secs(120);
    for (&id, &at) in previous
        .recent_qobuz
        .iter()
        .filter(|(_, at)| online && recent(at))
    {
        if !next.playlists.iter().any(|p| p.id == id) {
            if let Some(row) = previous.playlists.iter().find(|p| p.id == id) {
                next.playlists.push(row.clone());
            }
        }
        next.recent_qobuz.insert(id, at);
    }
}
/// Session-only folder expand state (matches Tauri — not persisted).
static EXPANDED: std::sync::LazyLock<Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));
/// (option, asc) — session scope (sidebar.rs SORT; Tauri persists in
/// localStorage, Slint keeps it in-session).
static SORT: std::sync::LazyLock<Mutex<(String, bool)>> =
    std::sync::LazyLock::new(|| Mutex::new(("name".to_string(), true)));
static SEARCH: Mutex<String> = Mutex::new(String::new());

/// Logout: drop the outgoing user's tree and the session-scoped view state.
///
/// `do_logout` publishes `"[]"` into the QML property, but that is only the
/// rendered document — this CACHE is what every `crate::publish_sidebar()`
/// rebuilds from (the Playlist Manager's optimistic move-to-folder patches it
/// in place and republishes without any fetch), so leaving it populated makes
/// the previous account's playlists, folders, folder membership, hidden set and
/// local rows re-appear for the next one. Same reasoning as `fav_cache_qt` /
/// `myqbz_*` / `artist_blacklist` teardowns in `auth_qt::logout`.
///
/// The expand set and the search box are session state and go with it. SORT
/// stays: it is a view preference, holds no user data, and the reference keeps
/// it for the process too.
pub fn teardown() {
    *CACHE.lock().unwrap() = None;
    EXPANDED.lock().unwrap().clear();
    SEARCH.lock().unwrap().clear();
    if let Ok(mut service) = PINNED.lock() {
        *service = None;
    }
    if let Ok(mut dir) = USER_DIR.lock() {
        *dir = None;
    }
    log::info!("[qbz-qt] sidebar and pinned stores cleared (logout)");
}

/// Fetch playlists (Qobuz) + folders + membership + hidden set (library.db).
///
/// Offline, the remote half is synthesized from local sidecars plus persisted
/// playlist membership that intersects the ready download cache. This is what
/// keeps only playable Qobuz playlists — and therefore only useful folders —
/// visible across both a connectivity flip and a cold app restart.
pub async fn load(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let offline = crate::offline_fwd::engine().status().is_offline();
    let playlists: Vec<SidebarPlaylist> = if offline {
        log::info!("[qbz-qt] sidebar load: offline — Qobuz fetch skipped");
        Vec::new()
    } else {
        log::debug!("[qbz-qt] sidebar load: fetching user playlists");
        match runtime.core().get_user_playlists().await {
            Ok(pls) => {
                // Ownership snapshot FIRST — every PlaylistCard's tri-state
                // overlay reads it, this is the earliest point in the session
                // where the user's own list exists, and `authoritative_entries`
                // below depends on the sets this call seeds.
                let pairs: Vec<(u64, u64)> = pls.iter().map(|p| (p.id, p.owner.id)).collect();
                crate::playlist_qt::set_user_playlists(&pairs);
                if let Some(entries) = crate::playlist_snapshot_qt::authoritative_entries(&pls) {
                    crate::playlist_snapshot_qt::record_authoritative_detached(entries);
                }
                pls
            }
            Err(e) => {
                log::warn!("[qbz-qt] sidebar playlists load failed: {e}");
                Vec::new()
            }
        }
        .into_iter()
        .map(|p| {
            // A custom playlist cover replaces the sidebar mosaic too.
            let custom = crate::cover_artwork_qt::playlist_cover(&p.id.to_string())
                .filter(|path| std::path::Path::new(path).is_file());
            let cover_urls = if let Some(custom_path) = custom {
                vec![custom_path]
            } else {
                let source = [&p.images300, &p.images150, &p.images]
                    .into_iter()
                    .flatten()
                    .find(|v| !v.is_empty());
                let mut out: Vec<String> = Vec::new();
                if let Some(list) = source {
                    for url in list {
                        if !url.is_empty() && !out.contains(url) {
                            out.push(url.clone());
                        }
                        if out.len() == 4 {
                            break;
                        }
                    }
                }
                out
            };
            SidebarPlaylist {
                id: p.id,
                name: p.name,
                tracks_count: p.tracks_count,
                cover_urls,
                position: 0,
            }
        })
        .collect()
    };

    log::debug!("[qbz-qt] sidebar load: playlists fetch settled, reading folders");
    let (folders, folder_map, positions, hidden_playlists, local_counts) = folders_blocking();
    log::debug!("[qbz-qt] sidebar load: folders read");
    let mut playlists = playlists;
    if offline {
        let prior: HashMap<u64, SidebarPlaylist> = CACHE
            .lock()
            .unwrap()
            .as_ref()
            .map(|data| {
                data.playlists
                    .iter()
                    .cloned()
                    .map(|playlist| (playlist.id, playlist))
                    .collect()
            })
            .unwrap_or_default();
        let headers = crate::playlist_snapshot_qt::headers_blocking();
        let available = crate::playlist_snapshot_qt::available_offline_blocking();
        let mut ids: Vec<u64> = local_counts
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(id, _)| *id)
            .collect();
        for id in available {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids.sort_unstable();
        for id in ids {
            let local_count = local_counts.get(&id).copied().unwrap_or(0);
            if let Some(mut cached) = prior.get(&id).cloned() {
                if let Some((name, track_count)) = headers.get(&id) {
                    cached.name = name.clone();
                    cached.tracks_count = track_count.unwrap_or(local_count);
                }
                playlists.push(cached);
                continue;
            }
            let (name, tracks_count) = headers
                .get(&id)
                .map(|(name, track_count)| (name.clone(), track_count.unwrap_or(local_count)))
                .unwrap_or_else(|| {
                    (
                        qbz_i18n::t_args("Playlist ({} local)", &[&local_count.to_string()]),
                        local_count,
                    )
                });
            playlists.push(SidebarPlaylist {
                id,
                name,
                tracks_count,
                cover_urls: Vec::new(),
                position: 0,
            });
        }
    }
    for p in &mut playlists {
        // Assign, don't merge: a preserved cached row carries its previous
        // position, and a settings row that has since been deleted must reset
        // it to 0 rather than leave the stale value in place.
        p.position = positions.get(&p.id).copied().unwrap_or(0);
    }
    // First-class LOCAL playlists (D7). Read AFTER the Qobuz fetch settles but
    // independently of it: the fetch can fail, or be gate-refused offline, and
    // the local set must still list — for a user without Qobuz it is the ONLY
    // set. Hidden locals drop here the way hidden Qobuz playlists do (B3).
    // Covers resolve with no network, from the playlist's own tracks.
    let locals: Vec<SidebarLocal> = crate::local_playlist_qt::list_blocking()
        .into_iter()
        .filter(|p| !p.hidden)
        .map(|p| SidebarLocal {
            covers: crate::local_playlist_qt::resolve_cover_urls_blocking(&p.id, 4),
            id: p.id,
            name: p.name,
            folder_id: p.folder_id,
        })
        .collect();
    log::debug!("[qbz-qt] sidebar load: {} local playlist(s)", locals.len());

    let (n_playlists, n_folders) = (playlists.len(), folders.len());
    crate::library_qt::with_deleted_playlists(|deleted| {
        playlists.retain(|p| !deleted.contains(&p.id.to_string()));
        let mut next = SidebarData {
            playlists,
            folders,
            folder_map,
            hidden_playlists,
            locals,
            ..Default::default()
        };
        let mut cache = CACHE.lock().unwrap();
        if let Some(previous) = cache.as_ref() {
            preserve_recent_entries(previous, &mut next, std::time::Instant::now(), !offline);
        }
        next.playlists
            .retain(|p| !deleted.contains(&p.id.to_string()));
        *cache = Some(next);
    });
    log::info!("[qbz-qt] sidebar loaded: {n_playlists} playlists, {n_folders} folders");
}

/// library.db queries (folders.rs equivalents), run inline — rusqlite is
/// sync but the three reads are tiny.
fn folders_blocking() -> (
    Vec<(String, String)>,
    HashMap<u64, String>,
    HashMap<u64, i32>,
    HashSet<u64>,
    HashMap<u64, u32>,
) {
    // Guest/offline-only users live in users/0, the same convention as every
    // other Qt library.db accessor.
    let uid = UserDataPaths::load_last_user_id().unwrap_or(0);
    let Some(path) = dirs::data_dir().map(|p| {
        p.join("qbz")
            .join("users")
            .join(uid.to_string())
            .join("library.db")
    }) else {
        return Default::default();
    };
    if !path.exists() {
        return Default::default();
    }
    let Ok(db) = qbz_library::LibraryDatabase::open(&path) else {
        return Default::default();
    };
    let folders: Vec<(String, String)> = db
        .get_all_playlist_folders()
        .unwrap_or_default()
        .into_iter()
        .filter(|f| !f.is_hidden)
        .map(|f| (f.id, f.name))
        .collect();
    let mut folder_map = HashMap::new();
    let mut positions = HashMap::new();
    let mut hidden = HashSet::new();
    for s in db.get_all_playlist_settings().unwrap_or_default() {
        if let Some(fid) = s.folder_id {
            folder_map.insert(s.qobuz_playlist_id, fid);
        }
        positions.insert(s.qobuz_playlist_id, s.position);
        if s.hidden {
            hidden.insert(s.qobuz_playlist_id);
        }
    }
    let local_counts = db.get_all_playlist_local_track_counts().unwrap_or_default();
    (folders, folder_map, positions, hidden, local_counts)
}

/// A Qobuz playlist's track count from the session cache (`sidebar.rs:389`).
///
/// This is the QOBUZ BLOCK SIZE — the base a library.db sidecar position is
/// computed from (`next_playlist_sidecar_position`), when a local/Plex ref is
/// attached to a Qobuz playlist. `None` when the sidebar has not loaded yet or
/// the playlist is not one of the user's; the caller then treats the block as
/// empty, and the sidecar's own `MAX(position) + 1` still keeps the batch past
/// every stored slot.
pub fn playlist_track_count(id: u64) -> Option<u32> {
    CACHE
        .lock()
        .ok()?
        .as_ref()?
        .playlists
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.tracks_count)
}

/// SidebarPlaylist order for the active sort (sidebar.rs comparators:
/// `recent`/`playcount` keep API order = newest-first; asc==true is always
/// the natural direction).
fn sort_playlists(playlists: &[SidebarPlaylist]) -> Vec<SidebarPlaylist> {
    let (sort, asc) = SORT.lock().unwrap().clone();
    let mut out: Vec<SidebarPlaylist> = playlists.to_vec();
    match sort.as_str() {
        "name" => out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
        "tracks" => out.sort_by(|a, b| b.tracks_count.cmp(&a.tracks_count)),
        "custom" => out.sort_by(|a, b| a.position.cmp(&b.position)),
        _ => {}
    }
    if !asc {
        out.reverse();
    }
    out
}

/// Rebuild the flattened entries from cache + expand + search and return
/// them (the caller publishes).
pub fn rebuild() -> Vec<SidebarEntry> {
    let Some(data) = CACHE.lock().unwrap().clone() else {
        return Vec::new();
    };
    let expanded = EXPANDED.lock().unwrap().clone();
    let query = SEARCH.lock().unwrap().clone();
    let searching = !query.is_empty();
    let offline = crate::offline_fwd::engine().status().is_offline();
    let folder_ids: HashSet<&String> = data.folders.iter().map(|f| &f.0).collect();

    let sorted = sort_playlists(&data.playlists);
    let matches =
        |p: &SidebarPlaylist| !searching || p.name.to_lowercase().contains(query.as_str());

    let entry_for = |p: &SidebarPlaylist, indent: bool, folder_id: &str| SidebarEntry {
        kind: "playlist".into(),
        id: p.id.to_string(),
        name: p.name.clone(),
        expanded: false,
        count: 0,
        indent,
        folder_id: folder_id.to_string(),
        covers: p.cover_urls.clone(),
        is_local: false,
    };
    let local_entry_for = |p: &SidebarLocal, indent: bool, folder_id: &str| SidebarEntry {
        kind: "playlist".into(),
        id: p.id.clone(),
        name: p.name.clone(),
        expanded: false,
        count: 0,
        indent,
        folder_id: folder_id.to_string(),
        covers: p.covers.clone(),
        is_local: true,
    };
    let local_matches =
        |p: &SidebarLocal| !searching || p.name.to_lowercase().contains(query.as_str());
    /// Locals sort among THEMSELVES by name, always — they have no
    /// track-count or custom-position stat to honour the toolbar's other
    /// sorts with, and inventing one would order them arbitrarily.
    fn sorted_locals<'a>(mut list: Vec<&'a SidebarLocal>) -> Vec<&'a SidebarLocal> {
        list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        list
    }

    let mut entries: Vec<SidebarEntry> = Vec::new();
    for (fid, fname) in &data.folders {
        let members: Vec<&SidebarPlaylist> = sorted
            .iter()
            .filter(|p| {
                data.folder_map
                    .get(&p.id)
                    .map(|f| f == fid)
                    .unwrap_or(false)
            })
            .filter(|p| matches(p))
            .filter(|p| !data.hidden_playlists.contains(&p.id))
            .collect();
        // A folder holds BOTH kinds: local membership rides
        // `local_playlists.folder_id`, pointing at this same folder.
        let local_members: Vec<&SidebarLocal> = sorted_locals(
            data.locals
                .iter()
                .filter(|p| p.folder_id.as_deref() == Some(fid.as_str()))
                .filter(|p| local_matches(p))
                .collect(),
        );
        // Search hides empty result branches. Offline mode additionally hides
        // folders with no playable member; Playlist Manager still reads every
        // folder directly from library.db and therefore remains complete.
        if (searching || offline) && members.is_empty() && local_members.is_empty() {
            continue;
        }
        // When searching, force-expand so matches inside are visible.
        let is_exp = searching || expanded.contains(fid);
        entries.push(SidebarEntry {
            kind: "folder".into(),
            id: fid.clone(),
            name: fname.clone(),
            expanded: is_exp,
            count: (members.len() + local_members.len()) as i32,
            indent: false,
            folder_id: String::new(),
            covers: Vec::new(),
            is_local: false,
        });
        if is_exp {
            for p in members {
                entries.push(entry_for(p, true, fid));
            }
            for p in local_members {
                entries.push(local_entry_for(p, true, fid));
            }
        }
    }
    // Root playlists — no folder, or a folder that no longer exists.
    for p in &sorted {
        let in_folder = data
            .folder_map
            .get(&p.id)
            .map(|f| folder_ids.contains(f))
            .unwrap_or(false);
        if !in_folder && matches(p) && !data.hidden_playlists.contains(&p.id) {
            entries.push(entry_for(p, false, ""));
        }
    }
    // LOCAL playlists not in a folder (or in one that no longer exists) —
    // root rows AFTER the Qobuz set, name-sorted, honouring the same search.
    // Always present, online or offline.
    for p in sorted_locals(
        data.locals
            .iter()
            .filter(|p| {
                let in_folder = p
                    .folder_id
                    .as_ref()
                    .map(|f| folder_ids.contains(f))
                    .unwrap_or(false);
                !in_folder && local_matches(p)
            })
            .collect(),
    ) {
        entries.push(local_entry_for(p, false, ""));
    }
    entries
}

/// PlaylistView-style sort toggle (#657): re-pick flips direction, a new
/// option starts at its natural direction (asc == true everywhere).
pub fn set_sort(option: &str) {
    let opt = match option {
        "name" | "recent" | "tracks" | "playcount" | "custom" => option,
        _ => "name",
    };
    let mut s = SORT.lock().unwrap();
    if s.0 == opt {
        s.1 = !s.1;
    } else {
        s.0 = opt.to_string();
        s.1 = true;
    }
}

pub fn sort_state() -> (String, bool) {
    SORT.lock().unwrap().clone()
}

pub fn set_search(query: &str) {
    *SEARCH.lock().unwrap() = query.trim().to_lowercase();
}

/// Toggle a folder's expand state (or set it explicitly for the tree's
/// collapse-all). Returns the new state.
pub fn toggle_folder(id: &str) -> bool {
    let mut expanded = EXPANDED.lock().unwrap();
    if !expanded.insert(id.to_string()) {
        expanded.remove(id);
        false
    } else {
        true
    }
}

// ---------------------------------------------------------------------------
// Cache accessors for the Playlist Manager seam (contract D10 / §5.2 / §5.6)
//
// All four answer from the SESSION CACHE only — no DB, no network — so they
// are callable from the Qt thread and they behave identically offline. Each
// returns `None` / an empty vector when the sidebar has never loaded, and each
// caller has a documented fallback for that state rather than treating it as
// an error.
// ---------------------------------------------------------------------------

/// One row of the mini-rail folder flyout (`QbzShell.sidebarFolderPopupJson`).
#[derive(Clone, Serialize)]
pub struct FolderPopupRow {
    pub id: String,
    pub name: String,
    #[serde(rename = "isLocal")]
    pub is_local: bool,
}

/// The playlists inside `folder_id`, in the order the expanded tree would show
/// them: Qobuz members under the active sort with hidden ones dropped, then
/// that folder's locals appended name-sorted (mirrors `sidebar.rs:517-546`).
///
/// The search filter is deliberately NOT applied — the flyout is opened from a
/// rail with no search field, and the reference's own header count comes from
/// the entry row, which is computed post-search. Here `count = rows.len()`, so
/// the header and the list can never disagree.
///
/// Consumed by `crate::sidebar_open_folder_popup` (block 6), which serialises
/// the rows into `QbzShell.sidebarFolderPopupJson`.
pub fn folder_popup_rows(folder_id: &str) -> Vec<FolderPopupRow> {
    let Some(data) = CACHE.lock().unwrap().clone() else {
        return Vec::new();
    };
    let mut rows: Vec<FolderPopupRow> = sort_playlists(&data.playlists)
        .into_iter()
        .filter(|p| {
            data.folder_map
                .get(&p.id)
                .map(|f| f == folder_id)
                .unwrap_or(false)
        })
        .filter(|p| !data.hidden_playlists.contains(&p.id))
        .map(|p| FolderPopupRow {
            id: p.id.to_string(),
            name: p.name,
            is_local: false,
        })
        .collect();
    let mut locals: Vec<&SidebarLocal> = data
        .locals
        .iter()
        .filter(|p| p.folder_id.as_deref() == Some(folder_id))
        .collect();
    locals.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    rows.extend(locals.into_iter().map(|p| FolderPopupRow {
        id: p.id.clone(),
        name: p.name.clone(),
        is_local: true,
    }));
    rows
}

/// A folder's display name from the session cache (`None` when the sidebar has
/// never loaded, or when the id names a HIDDEN folder — `SidebarData.folders`
/// is the already-filtered visible set, D11).
///
/// Only `crate::sidebar_open_folder_popup` calls it, to fill the `folderName`
/// key of the §4.7 document. The FLYOUT does not read that key — it takes the
/// name synchronously from the clicked entry, because the document lands a
/// later event-loop turn — so a `None` here degrades to `""` in a field nobody
/// renders rather than to a blank header.
pub fn folder_name(folder_id: &str) -> Option<String> {
    CACHE
        .lock()
        .ok()?
        .as_ref()?
        .folders
        .iter()
        .find(|(id, _)| id == folder_id)
        .map(|(_, name)| name.clone())
}

/// Insert an OPTIMISTIC Qobuz playlist row at the top of the sidebar cache —
/// the port of `crates/qbz/src/sidebar.rs:437-454`.
///
/// The playlist importer creates playlists through the API, and the
/// user-playlists endpoint lags the write by seconds: without this the newly
/// imported playlist is simply absent from the tree until the bounded-retry
/// reload catches up, and the user's only evidence it exists is a toast. It is
/// a no-op when the id is already cached, so the retry loop can call it again
/// after a failed reload without duplicating.
///
/// `covers` is the 2x2 micro-collage. The importer has none to give and passes
/// `&[]` (the collage fills on the next real load, which is what the reference
/// does unconditionally); the FOLLOW seam does have them — the open playlist
/// document already resolved the same urls — and passing them keeps the row
/// from appearing as a blank tile for the rest of the session.
///
/// Cache-only, like every accessor in this block: the caller publishes
/// (`crate::publish_sidebar()`), which keeps this correct offline and free of a
/// round trip.
pub fn insert_qobuz_entry(id: u64, name: &str, tracks_count: u32, covers: &[String]) {
    let mut guard = CACHE.lock().unwrap();
    // `None` = the sidebar has never loaded this session. The reference's cache
    // is not an Option and always has somewhere to insert; synthesising an
    // empty one here is the same behaviour — there is nothing yet for the
    // single row to hide, and the retry loop's `load()` replaces it wholesale.
    let data = guard.get_or_insert_with(SidebarData::default);
    if data.playlists.iter().any(|p| p.id == id) {
        return;
    }
    data.playlists.insert(
        0,
        SidebarPlaylist {
            id,
            name: name.to_string(),
            tracks_count,
            cover_urls: covers.iter().take(4).cloned().collect(),
            position: 0,
        },
    );
    data.recent_qobuz.insert(id, std::time::Instant::now());
    INSERT_REVISION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn insert_local_entry(id: &str, name: &str) {
    let mut cache = CACHE.lock().unwrap();
    let data = cache.get_or_insert_with(SidebarData::default);
    if data.locals.iter().any(|p| p.id == id) {
        return;
    }
    data.locals.push(SidebarLocal {
        id: id.to_string(),
        name: name.to_string(),
        folder_id: None,
        covers: Vec::new(),
    });
    INSERT_REVISION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Drop a Qobuz playlist row from the sidebar cache. Returns whether anything
/// was actually removed, so the caller can skip a pointless republish.
///
/// The twin of [`insert_qobuz_entry`], and it exists for the same reason the
/// insert does — Qobuz's `playlist/getUserPlaylists` lags a write. UNFOLLOWING
/// a playlist removes it from that list server-side, but a `reload_sidebar()`
/// fired immediately after the unsubscribe re-reads the STALE list and puts the
/// row straight back: the owner's report is literally "I unfollow it and it is
/// still in the sidebar". Patching the cache and republishing from it is the
/// D10 refresh — instant, correct offline, no round trip — and the next natural
/// load reconciles.
///
/// Folder membership, hidden flag and custom position are deliberately left
/// alone: they are per-playlist SETTINGS in library.db, and a re-follow should
/// find the playlist where the user filed it.
pub fn remove_qobuz_entry(id: u64) -> bool {
    let mut guard = CACHE.lock().unwrap();
    let Some(data) = guard.as_mut() else {
        return false;
    };
    let before = data.playlists.len();
    data.recent_qobuz.remove(&id);
    data.playlists.retain(|p| p.id != id);
    before != data.playlists.len()
}

/// Patch a playlist's folder membership in the sidebar cache — `""` = root.
///
/// The Playlist Manager's move-to-folder calls this and then
/// `crate::publish_sidebar()`, instead of a reload: the reload verb that works
/// offline still re-reads the whole DB, and the network one is a no-op offline
/// (D10). Both id kinds are handled — a Qobuz id patches `folder_map`, a
/// `local:` id patches the local row's own `folder_id` column mirror.
pub fn move_playlist_optimistic(id: &str, folder_id: &str) {
    let mut guard = CACHE.lock().unwrap();
    let Some(data) = guard.as_mut() else {
        return;
    };
    let target = (!folder_id.is_empty()).then(|| folder_id.to_string());
    use crate::local_playlist_qt::PlaylistRef;
    match PlaylistRef::parse(id) {
        Some(PlaylistRef::Local(local)) => {
            if let Some(p) = data.locals.iter_mut().find(|p| p.id == local) {
                p.folder_id = target;
            }
        }
        Some(PlaylistRef::Qobuz(pid)) => match target {
            Some(fid) => {
                data.folder_map.insert(pid, fid);
            }
            None => {
                data.folder_map.remove(&pid);
            }
        },
        None => {}
    }
}

/// A Qobuz playlist's name from the session cache.
///
/// The offline-synthesis fallback name for the Playlist Manager (§5.2): a
/// playlist that only exists offline as "some id with local sidecar rows" is
/// named from whatever the sidebar loaded while online, else from
/// `"Playlist ({} local)"`.
///
/// It CANNOT supply a description — `SidebarPlaylist` has no such field, unlike
/// the reference's `sidebar::playlist_name_desc` — so the playlist editor
/// resolves descriptions elsewhere and never through this.
pub fn playlist_name(id: u64) -> Option<String> {
    CACHE
        .lock()
        .ok()?
        .as_ref()?
        .playlists
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.name.clone())
}

/// A LOCAL playlist's already-resolved cover urls from the session cache.
///
/// `resolve_cover_urls_blocking` opens the per-user DB per call, so the manager
/// loader asks here FIRST and only falls back to the repo for the locals the
/// cache cannot answer (§5.6). `None` means "not in the cache" — an empty
/// `Some(vec![])` means "cached, and it genuinely has no covers".
pub fn local_covers(id: &str) -> Option<Vec<String>> {
    CACHE
        .lock()
        .ok()?
        .as_ref()?
        .locals
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.covers.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_store_dir(name: &str) -> std::path::PathBuf {
        static NONCE: AtomicU64 = AtomicU64::new(0);
        let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "qbz-qt-sidebar-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    /// Pinned local content belongs to the profile that created it. Teardown
    /// releases the handle, and reopening the guest profile restores its rows.
    #[test]
    fn pinned_store_rebinds_between_guest_and_account_profiles() {
        let guest = unique_store_dir("guest");
        let account = unique_store_dir("account");
        std::fs::create_dir_all(&guest).expect("guest dir");
        std::fs::create_dir_all(&account).expect("account dir");

        init_pinned(&guest);
        assert_eq!(user_dir().as_deref(), Some(guest.as_path()));
        assert_eq!(
            toggle_pin("album", "local:guest", "Guest Album", "", ""),
            Some(true)
        );
        assert!(is_pinned("album", "local:guest"));

        init_pinned(&account);
        assert_eq!(user_dir().as_deref(), Some(account.as_path()));
        assert!(!is_pinned("album", "local:guest"));

        teardown();
        assert!(user_dir().is_none());
        assert!(!is_pinned("album", "local:guest"));

        init_pinned(&guest);
        assert!(is_pinned("album", "local:guest"));

        teardown();
        let _ = std::fs::remove_dir_all(&guest);
        let _ = std::fs::remove_dir_all(&account);
    }
}

#[cfg(test)]
mod creation_regressions {
    use super::*;

    #[test]
    fn stale_list_preserves_new_playlist_without_duplicates_or_permanent_ghosts() {
        let now = std::time::Instant::now();
        let mut previous = SidebarData::default();
        previous.playlists.push(SidebarPlaylist {
            id: 42,
            name: "Created".into(),
            tracks_count: 0,
            cover_urls: vec![],
            position: 0,
        });
        previous.recent_qobuz.insert(42, now);
        let mut stale = SidebarData::default();
        preserve_recent_entries(&previous, &mut stale, now, true);
        assert_eq!(stale.playlists.len(), 1);
        preserve_recent_entries(&previous, &mut stale, now, true);
        assert_eq!(stale.playlists.len(), 1);
        let mut confirmed = stale.clone();
        confirmed.playlists[0].name = "Renamed remotely".into();
        preserve_recent_entries(&previous, &mut confirmed, now, true);
        assert_eq!(confirmed.playlists[0].name, "Renamed remotely");
        let mut offline = SidebarData::default();
        preserve_recent_entries(&previous, &mut offline, now, false);
        assert!(
            offline.playlists.is_empty(),
            "uncached cloud rows stay hidden offline"
        );
        let mut expired = SidebarData::default();
        preserve_recent_entries(
            &previous,
            &mut expired,
            now + std::time::Duration::from_secs(120),
            true,
        );
        assert!(expired.playlists.is_empty());
        previous.playlists.clear();
        previous.recent_qobuz.remove(&42);
        let mut deleted = SidebarData::default();
        preserve_recent_entries(&previous, &mut deleted, now, true);
        assert!(deleted.playlists.is_empty());
    }
}
