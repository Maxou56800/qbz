//! Playlist Manager, controller part 1 — the load, the cache, the toolbar
//! state and the two publish paths.
//!
//! Port of `crates/qbz/src/playlist_manager.rs`'s `load` / `apply` / `rebuild`
//! / `reset_session` / `navigate` plus the `PlaylistManagerActions` toolbar
//! handlers in the reference's `main.rs:5230-5330`. The pure model functions
//! live in `playlist_manager_rows`; the mutations live in
//! `playlist_manager_ops`.
//!
//! # The two publish paths are NOT the same function (§5.4)
//!
//! | path | writes | writes `pmLoading`? |
//! |---|---|---|
//! | [`publish_document`] | `managerJson` + `foldersJson` | **NO** |
//! | [`publish_loaded`] (the load's terminal rebuild) | both | yes — `false` |
//!
//! `navigate()` and `reload()` each set `pmLoading = true`; only the load
//! clears it. Conflating them is not a style question: the reference's
//! `rebuild()` ends with an unconditional `set_loading(false)` and gets away
//! with it because its `navigate` publishes NOTHING. Copy "publish always
//! clears loading" while also publishing from `navigate()`, and the `false`
//! lands in the same event-loop turn as the `true` — the spinner never renders,
//! the whole network+DB load runs with it off, and the empty-state branch
//! (`!loading && playlists.length === 0`) flashes "No playlists found." on
//! every single entry.
//!
//! # `foldersJson` is cache-INDEPENDENT (D5 / §5.3)
//!
//! The sidebar's row context menu reads `foldersJson` in sessions where the
//! manager view has never been opened, so the folder list cannot be a function
//! of the manager cache. [`refresh_folders`] reads the DB directly and never
//! consults `CACHE`; [`FOLDERS_CACHE`] holds the result, and a warm manager
//! cache refreshes it in memory (its counts then track the optimistic patches).
//! `managerJson.folderCount` is derived from the SAME vector in the SAME `ui()`
//! closure, so the counter row and the folder array can never disagree.
//!
//! # Everything reachable from an invokable is Qt-thread-safe
//!
//! `LibraryDatabase` wraps a `!Send` rusqlite connection and every `folders_qt`
//! / `local_playlist_qt` call opens the DB, so none of them may run on the Qt
//! thread. An invokable here only ever mutates a `Mutex`, publishes, and
//! `crate::spawn`s the rest (§5.19). [`publish_document`] in particular NEVER
//! touches the DB.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use cxx_qt_lib::QString;
use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use serde::Serialize;

use crate::folders_qt;
use crate::playlist_manager_rows as rows;
use crate::playlist_manager_rows::{
    FolderRow, PlaylistRow, PmData, PmLocalPlaylist, PmPlaylist, TreeRow,
};

type Runtime = Arc<AppRuntime<LoggingAdapter>>;

// ---------------------------------------------------------------------------
// Session state
// ---------------------------------------------------------------------------

/// Last-loaded data. `None` means **never loaded this session** — the one
/// condition `reload_if_loaded()` is allowed to test, and the one every
/// mutation is FORBIDDEN to test before its DB write (§5.3).
static CACHE: Mutex<Option<PmData>> = Mutex::new(None);

/// The canonical published folder array. Filled by [`refresh_folders`] from
/// the DB and by [`build_document`] from a warm cache; it is what
/// `foldersJson` always carries, so a cold-cache publish can never clobber it
/// with `[]`.
static FOLDERS_CACHE: LazyLock<Mutex<Vec<FolderRow>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Session tree-expand state. Never reset — only the latch below is, so a
/// re-entry re-expands every folder on top of whatever the user collapsed.
/// That is the reference behaviour (§5.5); keep it.
static EXPANDED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// True once the tree has auto-expanded folders since the last `navigate()`.
static TREE_INIT: Mutex<bool> = Mutex::new(false);

/// Debounce generation for the search field (D8). A publish only happens for
/// the newest keystroke.
static SEARCH_GEN: AtomicU64 = AtomicU64::new(0);

/// Search debounce, milliseconds. Every OTHER toolbar setter publishes
/// immediately — only typing is coalesced, because each publish re-serialises
/// the whole document and tears down every visible card and collage.
const SEARCH_DEBOUNCE_MS: u64 = 150;

/// Session-scoped toolbar state. It SURVIVES leaving and re-entering the view
/// (§5.5) and resets only on restart, so `navigate()` must not touch it.
#[derive(Clone, Debug)]
struct Toolbar {
    /// SEED ONLY for the QML field — the text itself is QML-local (D8).
    search: String,
    /// `"all" | "visible" | "hidden"`.
    filter: String,
    /// `"name" | "recent" | "playcount" | "tracks" | "custom"`.
    sort: String,
    sort_asc: bool,
    /// `"grid" | "list" | "tree"`.
    view_mode: String,
    folder_mode: bool,
    folders_collapsed: bool,
    /// Grid/list drill-down. `None` is the root beside the folder cards;
    /// tree mode owns its own expand state and never reads this value.
    current_folder_id: Option<String>,
}

impl Default for Toolbar {
    fn default() -> Self {
        Self {
            search: String::new(),
            filter: "all".into(),
            sort: "name".into(),
            sort_asc: true,
            view_mode: "grid".into(),
            folder_mode: true,
            folders_collapsed: false,
            current_folder_id: None,
        }
    }
}

static TOOLBAR: LazyLock<Mutex<Toolbar>> = LazyLock::new(|| Mutex::new(Toolbar::default()));

// ---------------------------------------------------------------------------
// The wire document (§4.2)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagerDoc {
    search: String,
    filter: String,
    sort: String,
    sort_asc: bool,
    view_mode: String,
    folder_mode: bool,
    folders_collapsed: bool,
    current_folder_id: String,
    current_folder_name: String,
    can_reorder: bool,
    /// Always equal to `foldersJson.length` — same source, same closure (D5).
    folder_count: i32,
    /// POST-FILTER visible rows, locals included.
    playlist_count: i32,
    playlists: Vec<PlaylistRow>,
    /// EMPTY unless `folderMode && viewMode == "tree"`.
    tree: Vec<TreeRow>,
}

// ---------------------------------------------------------------------------
// Publishing
// ---------------------------------------------------------------------------

/// First-open auto-expand: the tree expands every folder the first time it is
/// opened after a `navigate()`. `reset_session` clears the latch, never
/// `EXPANDED` itself.
fn ensure_tree_init(data: &PmData) {
    let mut init = TREE_INIT.lock().unwrap_or_else(|e| e.into_inner());
    if *init {
        return;
    }
    if let Ok(mut exp) = EXPANDED.lock() {
        for f in &data.folders {
            exp.insert(f.id.clone());
        }
    }
    *init = true;
}

/// Serialise `(managerJson, foldersJson)` from the live cache + toolbar.
///
/// Pure with respect to I/O: no DB, no network. The only state it writes is
/// [`FOLDERS_CACHE`] (refreshed in memory from a warm manager cache) and the
/// tree auto-expand latch.
fn build_document() -> (String, String) {
    let cached = CACHE.lock().unwrap().clone();
    let warm = cached.is_some();
    let data = cached.unwrap_or_default();
    let tb = TOOLBAR.lock().unwrap().clone();
    let query = tb.search.trim().to_lowercase();
    let offline = crate::offline_fwd::engine().status().is_offline();

    // Warm cache: recompute so the counts track optimistic moves. Cold cache:
    // whatever `refresh_folders()` last read, NEVER an empty array — the
    // sidebar's move-to-folder list reads this in sessions with no manager.
    let folders = if warm {
        let fresh = rows::folder_rows(&data);
        *FOLDERS_CACHE.lock().unwrap() = fresh.clone();
        fresh
    } else {
        FOLDERS_CACHE.lock().unwrap().clone()
    };

    let current_folder = (tb.folder_mode && tb.view_mode != "tree")
        .then_some(tb.current_folder_id.as_deref())
        .flatten()
        .and_then(|id| data.folders.iter().find(|folder| folder.id == id));
    let (current_folder_id, current_folder_name) = current_folder
        .map(|folder| (folder.id.clone(), folder.name.clone()))
        .unwrap_or_default();
    let playlists = if current_folder_id.is_empty() {
        rows::visible_playlist_rows(
            &data,
            &query,
            &tb.filter,
            &tb.sort,
            tb.sort_asc,
            tb.folder_mode,
            &tb.view_mode,
            offline,
        )
    } else {
        rows::visible_folder_playlist_rows(
            &data,
            &current_folder_id,
            &query,
            &tb.filter,
            &tb.sort,
            tb.sort_asc,
            offline,
        )
    };

    let tree = if tb.folder_mode && tb.view_mode == "tree" {
        ensure_tree_init(&data);
        let expanded = EXPANDED.lock().unwrap().clone();
        rows::build_tree(
            &data,
            &query,
            &tb.filter,
            &tb.sort,
            tb.sort_asc,
            offline,
            &expanded,
        )
    } else {
        Vec::new()
    };

    let doc = ManagerDoc {
        search: tb.search.clone(),
        filter: tb.filter.clone(),
        sort: tb.sort.clone(),
        sort_asc: tb.sort_asc,
        view_mode: tb.view_mode.clone(),
        folder_mode: tb.folder_mode,
        folders_collapsed: tb.folders_collapsed,
        current_folder_id,
        current_folder_name,
        // D9: the DIRECTION is part of the gate. The reference omits it, and
        // under custom+descending its arrows renumber `position = 0..n` over
        // the REVERSED order — one press silently rewrites the user's whole
        // persisted custom order.
        can_reorder: tb.sort == "custom" && tb.sort_asc && query.is_empty(),
        folder_count: folders.len() as i32,
        playlist_count: playlists.len() as i32,
        playlists,
        tree,
    };

    (
        serde_json::to_string(&doc).unwrap_or_else(|_| "{}".into()),
        serde_json::to_string(&folders).unwrap_or_else(|_| "[]".into()),
    )
}

/// Publish the document. **Never touches `pmLoading`** (§5.4).
pub(crate) fn publish_document() {
    let deletion_revision = crate::library_qt::playlist_deletion_revision();
    let (manager, folders) = build_document();
    crate::playlist_manager_bridge::ui(move |mut b| {
        if deletion_revision != crate::library_qt::playlist_deletion_revision() {
            return;
        }
        b.as_mut().set_manager_json(QString::from(manager.as_str()));
        b.as_mut().set_folders_json(QString::from(folders.as_str()));
    });
}

/// The load's TERMINAL rebuild — the one and only place `pmLoading` is
/// cleared.
fn publish_loaded() {
    let deletion_revision = crate::library_qt::playlist_deletion_revision();
    let (manager, folders) = build_document();
    crate::playlist_manager_bridge::ui(move |mut b| {
        if deletion_revision != crate::library_qt::playlist_deletion_revision() {
            b.as_mut().set_pm_loading(false);
            return;
        }
        b.as_mut().set_manager_json(QString::from(manager.as_str()));
        b.as_mut().set_folders_json(QString::from(folders.as_str()));
        b.as_mut().set_pm_loading(false);
    });
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Seed the closed/default document and kick the cache-independent folder
/// read, so the sidebar's row menu has a folder list before the manager has
/// ever been opened (§5.19).
pub(crate) fn boot() {
    publish_document();
    refresh_folders();
}

/// Open the manager: record the route, clear the tree latch, raise the
/// spinner, publish the TOOLBAR so the view mounts with chrome instead of an
/// empty document. **No load** — the view's `Component.onCompleted` calls
/// [`reload`], which is also what serves the back/forward re-entry case
/// (`nav_qt::back()` republishes `currentView` and runs no per-view load, so
/// `navigate()` never ran). Loading from both would run `get_user_playlists()`
/// twice over the network on every entry (§5.11).
pub(crate) fn navigate() {
    crate::nav_qt::record("playlistmanager");
    reset_session();
    // Raised here, cleared ONLY by publish_loaded(). Written inline rather
    // than through a shared helper so the invariant is greppable: three
    // `set_pm_loading` call sites in the whole crate — this one, reload()'s,
    // and the single `false` at the end of the load.
    crate::playlist_manager_bridge::ui(|mut b| {
        b.as_mut().set_pm_loading(true);
    });
    publish_document();
}

/// Reset ONLY the tree-expand latch (§5.5). Search / filter / sort /
/// direction / view mode / folder mode / folders-collapsed all persist for the
/// session — do not reset them here.
fn reset_session() {
    *TREE_INIT.lock().unwrap_or_else(|e| e.into_inner()) = false;
}

/// The single load. Raises the spinner at entry too, because a Back-triggered
/// `Component.onCompleted` reaches here without `navigate()` having run.
pub(crate) fn reload() {
    crate::playlist_manager_bridge::ui(|mut b| {
        b.as_mut().set_pm_loading(true);
    });
    let runtime = crate::app();
    crate::spawn(async move {
        let mut data = load(&runtime).await;
        crate::library_qt::with_deleted_playlists(|deleted| {
            data.playlists
                .retain(|p| !deleted.contains(&p.id.to_string()));
            *CACHE.lock().unwrap() = Some(data);
        });
        publish_loaded();
    });
}

/// Refresh the manager after a write made ELSEWHERE (the folder editor, the
/// playlist editor, the sidebar row menu).
///
/// This early return is the ONLY thing in this controller allowed to be
/// conditional on the cache: when the manager has never been opened there is
/// no reader for the document, so the refetch would be pure cost. Generalising
/// it to the mutations turns "Hide from sidebar" and "Move to folder" into
/// silent no-ops for the account-less, offline population the whole feature
/// exists for (§5.3).
///
/// Callers: `folder_edit_qt::after_write` (block 2) and
/// `playlist_edit_qt::after_write` (block 3), after every successful write. It
/// lands here because the cross-singleton refresh seam is this controller's to
/// define, not theirs.
pub fn reload_if_loaded() {
    if CACHE.lock().unwrap().is_none() {
        return;
    }
    reload();
}

pub(crate) fn playlist_deleted(id: u64) {
    {
        let mut cache = CACHE.lock().unwrap();
        let Some(data) = cache.as_mut() else { return };
        data.playlists.retain(|p| p.id != id);
    }
    publish_document();
}

/// Name + description of a QOBUZ playlist out of the WARM manager cache.
///
/// Block 3's editor seeds from here before it falls back to
/// `core().get_playlist(pid)` (§5.2): the loader already fetched the
/// description and `PmPlaylist` carries it precisely so opening the editor
/// from the manager costs no request and works with the network down.
/// `None` means "the cache cannot answer" — never "no description"; a
/// playlist that genuinely has none answers `Some((name, None))`.
///
/// Deliberately NOT extended to `local:` ids: the local arm reads
/// `library.db` directly, which is always authoritative and always available.
pub(crate) fn cached_playlist_seed(id: u64) -> Option<(String, Option<String>)> {
    let guard = CACHE.lock().ok()?;
    let data = guard.as_ref()?;
    data.playlists
        .iter()
        .find(|p| p.id == id)
        .map(|p| (p.name.clone(), p.description.clone()))
}

/// Re-read `foldersJson` straight from the DB and republish.
///
/// Deliberately NOT gated on the manager cache (D5): a folder created from the
/// sidebar must reach `foldersJson` in a session where the manager view was
/// never opened. Counts come from `CACHE` when it is warm and from the DB when
/// it is not, so the number is never a placeholder 0 in either state.
pub fn refresh_folders() {
    crate::spawn(async move {
        let rows = tokio::task::spawn_blocking(folder_rows_blocking)
            .await
            .unwrap_or_default();
        *FOLDERS_CACHE.lock().unwrap() = rows;
        publish_document();
    });
}

/// The DB half of [`refresh_folders`]. Blocking — `spawn_blocking` only.
fn folder_rows_blocking() -> Vec<FolderRow> {
    let folders = folders_qt::load_folders_full();
    // Warm cache: its counts already fold in the optimistic patches, so reuse
    // them rather than re-reading a DB that may lag an in-flight write.
    let cached = CACHE.lock().unwrap().clone();
    if let Some(data) = cached.as_ref() {
        let counts = rows::folder_counts(data);
        return folders
            .iter()
            .map(|f| rows::folder_item(f, counts.get(&f.id).copied().unwrap_or(0)))
            .collect();
    }
    // Cold cache: count both kinds from the DB, or a folder holding only local
    // playlists reads "0 playlists" on its card (D6.3).
    let settings = folders_qt::playlist_settings_map();
    let locals = crate::local_playlist_qt::list_blocking();
    let live: HashSet<&String> = folders.iter().map(|f| &f.id).collect();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for s in settings.values() {
        if let Some(fid) = s.folder_id.as_ref().filter(|f| live.contains(f)) {
            *counts.entry(fid.clone()).or_insert(0) += 1;
        }
    }
    for p in &locals {
        if let Some(fid) = p.folder_id.as_ref().filter(|f| live.contains(f)) {
            *counts.entry(fid.clone()).or_insert(0) += 1;
        }
    }
    folders
        .iter()
        .map(|f| rows::folder_item(f, counts.get(&f.id).copied().unwrap_or(0)))
        .collect()
}

// ---------------------------------------------------------------------------
// Toolbar
// ---------------------------------------------------------------------------

/// Search-as-you-type. The query is stored IMMEDIATELY (so a publish triggered
/// by anything else already reflects it) and the rebuild is coalesced on a
/// 150 ms debounce (D8).
pub(crate) fn search_changed(query: &str) {
    TOOLBAR.lock().unwrap().search = query.to_string();
    let generation = SEARCH_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    crate::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(SEARCH_DEBOUNCE_MS)).await;
        if SEARCH_GEN.load(Ordering::SeqCst) == generation {
            publish_document();
        }
    });
}

pub(crate) fn set_filter(value: &str) {
    let valid = matches!(value, "all" | "visible" | "hidden");
    TOOLBAR.lock().unwrap().filter = if valid {
        value.to_string()
    } else {
        "all".into()
    };
    publish_document();
}

/// #657 direction toggle: re-selecting the ACTIVE option flips the direction;
/// picking a DIFFERENT one sets it and resets to its natural direction. Same
/// shape as `sidebar_qt::set_sort` — mirrored, not reinvented.
pub(crate) fn set_sort(value: &str) {
    let option = match value {
        "name" | "recent" | "playcount" | "tracks" | "custom" => value,
        _ => "name",
    };
    {
        let mut tb = TOOLBAR.lock().unwrap();
        if tb.sort == option {
            tb.sort_asc = !tb.sort_asc;
        } else {
            tb.sort = option.to_string();
            tb.sort_asc = true;
        }
    }
    publish_document();
}

pub(crate) fn set_view_mode(value: &str) {
    let mode = match value {
        "grid" | "list" | "tree" => value,
        _ => "grid",
    };
    let mut toolbar = TOOLBAR.lock().unwrap();
    toolbar.view_mode = mode.to_string();
    if mode == "tree" {
        toolbar.current_folder_id = None;
    }
    drop(toolbar);
    publish_document();
}

/// Leaving folder mode while in the tree falls back to grid — the tree IS the
/// folder surface, so `!folderMode && viewMode == "tree"` is unreachable.
pub(crate) fn toggle_folder_mode() {
    {
        let mut tb = TOOLBAR.lock().unwrap();
        tb.folder_mode = !tb.folder_mode;
        if !tb.folder_mode && tb.view_mode == "tree" {
            tb.view_mode = "grid".into();
        }
        if !tb.folder_mode {
            tb.current_folder_id = None;
        }
    }
    publish_document();
}

/// Collapse / expand the FOLDERS section.
///
/// The reference deliberately does NOT rebuild here (`main.rs:5313-5322`) —
/// the flag is a pure view property there. In this port the flag rides
/// `managerJson`, so it must be published; the rebuild that comes with it is
/// from the cache and hits neither the network nor the DB.
pub(crate) fn toggle_folders_collapsed() {
    {
        let mut tb = TOOLBAR.lock().unwrap();
        tb.folders_collapsed = !tb.folders_collapsed;
    }
    publish_document();
}

/// Enter one folder without changing the user's grid/list presentation.
/// Invalid/stale ids are ignored rather than publishing an inescapable empty
/// drill-down after a concurrent folder deletion.
pub(crate) fn open_folder(folder_id: &str) {
    let folder_id = folder_id.trim();
    if folder_id.is_empty() {
        return;
    }
    let exists = CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .is_some_and(|data| data.folders.iter().any(|folder| folder.id == folder_id));
    if !exists {
        return;
    }
    {
        let mut toolbar = TOOLBAR.lock().unwrap_or_else(|error| error.into_inner());
        toolbar.folder_mode = true;
        if toolbar.view_mode == "tree" {
            toolbar.view_mode = "grid".into();
        }
        toolbar.current_folder_id = Some(folder_id.to_string());
    }
    publish_document();
}

/// Return from a grid/list folder drill-down to the root folders + playlists.
pub(crate) fn close_folder() {
    TOOLBAR
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .current_folder_id = None;
    publish_document();
}

pub(crate) fn toggle_tree_folder(folder_id: &str) {
    {
        let mut expanded = EXPANDED.lock().unwrap();
        if !expanded.remove(folder_id) {
            expanded.insert(folder_id.to_string());
        }
    }
    publish_document();
}

// ---------------------------------------------------------------------------
// Cache access for `playlist_manager_ops`
// ---------------------------------------------------------------------------

/// Run `f` against the cache when it is WARM; `false` when it is cold.
///
/// The mutations use the return value to decide whether to republish — never
/// whether to write to the DB (§5.3).
pub(crate) fn patch_cache(f: impl FnOnce(&mut PmData)) -> bool {
    let mut guard = CACHE.lock().unwrap();
    match guard.as_mut() {
        Some(data) => {
            f(data);
            true
        }
        None => false,
    }
}

/// The ids of the currently visible QOBUZ rows, in the published order — the
/// vector `reorder_step` swaps in and renumbers.
///
/// Locals drop out here exactly as they do in the reference (which parses the
/// model's ids as `u64`): custom positions live in `playlist_settings`, whose
/// primary key is a Qobuz id, so a local has no slot to occupy (D29).
pub(crate) fn visible_qobuz_ids() -> Vec<u64> {
    let Some(data) = CACHE.lock().unwrap().clone() else {
        return Vec::new();
    };
    let tb = TOOLBAR.lock().unwrap().clone();
    let query = tb.search.trim().to_lowercase();
    let offline = crate::offline_fwd::engine().status().is_offline();
    let current_folder = (tb.folder_mode && tb.view_mode != "tree")
        .then_some(tb.current_folder_id.as_deref())
        .flatten()
        .filter(|id| data.folders.iter().any(|folder| folder.id == *id));
    let rows = if let Some(folder_id) = current_folder {
        rows::visible_folder_playlist_rows(
            &data,
            folder_id,
            &query,
            &tb.filter,
            &tb.sort,
            tb.sort_asc,
            offline,
        )
    } else {
        rows::visible_playlist_rows(
            &data,
            &query,
            &tb.filter,
            &tb.sort,
            tb.sort_asc,
            tb.folder_mode,
            &tb.view_mode,
            offline,
        )
    };
    rows.iter()
        .filter_map(|r| r.id.parse::<u64>().ok())
        .collect()
}

// ---------------------------------------------------------------------------
// The load
// ---------------------------------------------------------------------------

/// Fetch playlists (Qobuz) + folders + settings + stats + local counts + local
/// playlists, and merge them into the Send `PmData`.
///
/// **ONE `spawn_blocking`**, returning every DB read as a tuple; a join error
/// degrades the whole tuple to empties rather than panicking. Do not split it
/// into six hops — the DB is opened per call, and the point of the single hop
/// is to open it the minimum number of times per load.
///
/// There is no error state anywhere in this path: the fetch swallows its
/// failure into an empty vector and the join swallows its own, so a network
/// outage renders as "No playlists found." That is the reference (§5.13).
async fn load(runtime: &Runtime) -> PmData {
    let offline = crate::offline_fwd::engine().status().is_offline();
    let remote: Vec<qbz_models::Playlist> = if offline {
        Vec::new()
    } else {
        runtime
            .core()
            .get_user_playlists()
            .await
            .unwrap_or_else(|e| {
                log::warn!("[qbz-qt] playlist-manager playlists load failed: {e}");
                Vec::new()
            })
    };
    if !(offline && remote.is_empty()) {
        // A failed online fetch also lands here as an empty list; recording it
        // would start the two-generation retirement clock on every playlist,
        // so only a NON-empty list (or a genuinely deleted-everything account,
        // which still lists a session user) is recorded.
        if let Some(entries) = crate::playlist_snapshot_qt::authoritative_entries(&remote) {
            if !entries.is_empty() {
                crate::playlist_snapshot_qt::record_authoritative_detached(entries);
            }
        }
    }

    let (folders, settings, play_counts, local_counts, locals, fav_ids) =
        tokio::task::spawn_blocking(|| {
            // §5.6: locals get covers. Ask the sidebar cache FIRST — the work
            // is already done there — and only fall back to the repo for the
            // ones it cannot answer, because `resolve_cover_urls_blocking`
            // opens the DB per call.
            let locals: Vec<PmLocalPlaylist> = crate::local_playlist_qt::list_blocking()
                .into_iter()
                .map(|p| {
                    // The editor/header writes the Qt-owned override store.
                    // Read it BEFORE the sidebar cache so concurrent surface
                    // refreshes cannot republish the old mosaic here.
                    let custom = crate::cover_artwork_qt::playlist_cover(&p.id)
                        .filter(|path| std::path::Path::new(path).is_file());
                    let cover_urls = custom.map(|path| vec![path]).unwrap_or_else(|| {
                        crate::sidebar_qt::local_covers(&p.id).unwrap_or_else(|| {
                            crate::local_playlist_qt::resolve_cover_urls_blocking(&p.id, 4)
                        })
                    });
                    PmLocalPlaylist {
                        cover_urls,
                        id: p.id,
                        name: p.name,
                        offline_only: p.offline_only,
                        track_count: p.track_count,
                        is_favorite: p.favorite,
                        is_hidden: p.hidden,
                        folder_id: p.folder_id,
                    }
                })
                .collect();
            (
                folders_qt::load_folders_full(),
                folders_qt::playlist_settings_map(),
                folders_qt::playlist_play_counts(),
                folders_qt::playlist_local_counts(),
                locals,
                crate::library_db_qt::favorite_playlist_ids(),
            )
        })
        .await
        .unwrap_or_else(|_| {
            log::warn!("[qbz-qt] playlist-manager DB read join failed");
            Default::default()
        });

    let folder_ids: HashSet<&String> = folders.iter().map(|f| &f.id).collect();

    let mut playlists: Vec<PmPlaylist> = remote
        .iter()
        .map(|p| {
            let s = settings.get(&p.id).cloned().unwrap_or_default();
            PmPlaylist {
                id: p.id,
                name: p.name.clone(),
                description: p.description.clone(),
                tracks_count: p.tracks_count,
                duration: p.duration,
                local_count: local_counts.get(&p.id).copied().unwrap_or(0),
                play_count: play_counts.get(&p.id).copied().unwrap_or(0),
                is_favorite: s.is_favorite,
                is_hidden: s.hidden,
                // A folder that no longer exists falls back to root — the same
                // guard the sidebar applies (§5.18).
                folder_id: s.folder_id.filter(|fid| folder_ids.contains(fid)),
                // No settings row = PlaylistSettingsLite::default(): visible,
                // unfavourited, root, position 0 — so an unpositioned set is
                // one tie block that keeps API order under a stable sort.
                position: s.position,
                cover_urls: cover_urls(p),
            }
        })
        .collect();

    // INTERNAL favourites top-up (§5.18): a hearted playlist that is neither
    // owned nor subscribed never comes back from `get_user_playlists`, so
    // without this it is invisible here while Favorites > Playlists shows it.
    // `is_favorite: true` is the ONLY hardcoded field; everything else still
    // comes from the settings + count maps. Online only.
    if !offline {
        let known: HashSet<u64> = playlists.iter().map(|p| p.id).collect();
        for fid in fav_ids {
            if known.contains(&fid) {
                continue;
            }
            if let Ok(p) = runtime.core().get_playlist(fid).await {
                let s = settings.get(&fid).cloned().unwrap_or_default();
                playlists.push(PmPlaylist {
                    id: fid,
                    name: p.name.clone(),
                    description: p.description.clone(),
                    tracks_count: p.tracks_count,
                    duration: p.duration,
                    local_count: local_counts.get(&fid).copied().unwrap_or(0),
                    play_count: play_counts.get(&fid).copied().unwrap_or(0),
                    is_favorite: true,
                    is_hidden: s.hidden,
                    folder_id: s.folder_id.filter(|f| folder_ids.contains(f)),
                    position: s.position,
                    cover_urls: cover_urls(&p),
                });
            }
        }
    }

    // OFFLINE synthesis: the Qobuz fetch was skipped, so the reachable Qobuz
    // playlists are the MIXED ones — those with >= 1 local sidecar row. Named
    // from the sidebar's session cache (loaded while online) else the
    // "Playlist (N local)" fallback. The persisted snapshot subsystem that
    // would supply better names offline is absent from this port (D30), which
    // is why there is no third source here.
    if offline {
        let known: HashSet<u64> = playlists.iter().map(|p| p.id).collect();
        let mut ids: Vec<u64> = local_counts
            .iter()
            .filter(|&(_, &count)| count > 0)
            .map(|(&id, _)| id)
            .filter(|id| !known.contains(id))
            .collect();
        // A HashMap iterates in an arbitrary order; sort so the synthesised
        // block is stable across rebuilds (it is a tie block under `recent`).
        ids.sort_unstable();
        for id in ids {
            let count = local_counts.get(&id).copied().unwrap_or(0);
            let s = settings.get(&id).cloned().unwrap_or_default();
            playlists.push(PmPlaylist {
                id,
                name: crate::sidebar_qt::playlist_name(id).unwrap_or_else(|| {
                    qbz_i18n::t_args("Playlist ({} local)", &[&count.to_string()])
                }),
                description: None,
                tracks_count: 0,
                duration: 0,
                local_count: count,
                play_count: play_counts.get(&id).copied().unwrap_or(0),
                is_favorite: s.is_favorite,
                is_hidden: s.hidden,
                folder_id: s.folder_id.filter(|fid| folder_ids.contains(fid)),
                position: s.position,
                cover_urls: Vec::new(),
            });
        }
    }

    // Locals get the SAME dangling-folder guard as the Qobuz rows, so
    // `build_tree`'s "only unfiled locals go in the root run" can be a plain
    // `folder_id.is_none()` test.
    let mut locals = locals;
    for p in &mut locals {
        if p.folder_id
            .as_ref()
            .is_some_and(|f| !folder_ids.contains(f))
        {
            p.folder_id = None;
        }
    }

    log::info!(
        "[qbz-qt] playlist-manager loaded: {} playlists, {} folders, {} local",
        playlists.len(),
        folders.len(),
        locals.len()
    );
    PmData {
        playlists,
        folders,
        locals,
    }
}

/// Up to four de-duplicated cover urls.
///
/// `images300` → `images150` → `images`, FIRST NON-EMPTY LIST WINS, then skip
/// empties, skip duplicates, cap at 4. The reference's doc-comment claims
/// "images150 > images300" and is wrong about its own code; this matches the
/// code, and `sidebar_qt.rs:186-188`.
fn cover_urls(p: &qbz_models::Playlist) -> Vec<String> {
    // A user override is one full-bleed tile and must win in the manager just
    // as it already does in PlaylistView, Library and the sidebar.
    if let Some(path) = crate::cover_artwork_qt::playlist_cover(&p.id.to_string()) {
        if std::path::Path::new(&path).is_file() {
            return vec![path];
        }
    }
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
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The toolbar statics are process-global; these tests drive them through
    /// the public setters and re-derive expectations from the state they just
    /// set, so ordering between them cannot matter. The bridge publish hop is
    /// a no-op off the Qt thread (QT_THREAD unset).
    static LOCK: Mutex<()> = Mutex::new(());

    fn reset_toolbar() {
        *TOOLBAR.lock().unwrap() = Toolbar::default();
    }

    #[test]
    fn set_sort_flips_direction_on_reselect_and_resets_it_on_a_new_option() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_toolbar();
        assert_eq!(TOOLBAR.lock().unwrap().sort, "name");
        assert!(TOOLBAR.lock().unwrap().sort_asc);

        set_sort("name");
        assert!(!TOOLBAR.lock().unwrap().sort_asc, "re-select flips");
        set_sort("name");
        assert!(TOOLBAR.lock().unwrap().sort_asc, "and flips back");

        set_sort("name");
        set_sort("tracks");
        let tb = TOOLBAR.lock().unwrap().clone();
        assert_eq!(tb.sort, "tracks");
        assert!(tb.sort_asc, "a NEW option resets to its natural direction");

        // Unknown values fall back to "name" rather than poisoning the state.
        set_sort("nonsense");
        assert_eq!(TOOLBAR.lock().unwrap().sort, "name");
        reset_toolbar();
    }

    #[test]
    fn leaving_folder_mode_from_the_tree_falls_back_to_grid() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_toolbar();
        set_view_mode("tree");
        toggle_folder_mode();
        let tb = TOOLBAR.lock().unwrap().clone();
        assert!(!tb.folder_mode);
        assert_eq!(tb.view_mode, "grid");
        // Re-entering folder mode does NOT restore the tree.
        toggle_folder_mode();
        assert_eq!(TOOLBAR.lock().unwrap().view_mode, "grid");
        reset_toolbar();
    }

    #[test]
    fn setters_reject_unknown_values() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_toolbar();
        set_filter("hidden");
        assert_eq!(TOOLBAR.lock().unwrap().filter, "hidden");
        set_filter("bogus");
        assert_eq!(TOOLBAR.lock().unwrap().filter, "all");
        set_view_mode("bogus");
        assert_eq!(TOOLBAR.lock().unwrap().view_mode, "grid");
        reset_toolbar();
    }

    #[test]
    fn can_reorder_needs_custom_ascending_and_an_empty_query() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_toolbar();
        let can_reorder = || -> bool {
            let (manager, _) = build_document();
            let v: serde_json::Value = serde_json::from_str(&manager).unwrap();
            v["canReorder"].as_bool().unwrap()
        };
        assert!(!can_reorder(), "default sort is name");
        set_sort("custom");
        assert!(can_reorder());
        // D9: descending custom would renumber a REVERSED order.
        set_sort("custom");
        assert!(!can_reorder(), "custom + descending must not offer arrows");
        set_sort("custom");
        assert!(can_reorder());
        TOOLBAR.lock().unwrap().search = "  jazz ".into();
        assert!(!can_reorder(), "an active query must not offer arrows");
        TOOLBAR.lock().unwrap().search = "   ".into();
        assert!(can_reorder(), "a whitespace-only query is no query");
        reset_toolbar();
    }

    #[test]
    fn the_cold_document_is_parseable_and_folder_count_tracks_the_folder_array() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_toolbar();
        *CACHE.lock().unwrap() = None;
        *FOLDERS_CACHE.lock().unwrap() = vec![FolderRow {
            id: "f1".into(),
            name: "Jazz".into(),
            count: 4,
            ..Default::default()
        }];
        let (manager, folders) = build_document();
        let m: serde_json::Value = serde_json::from_str(&manager).unwrap();
        let f: serde_json::Value = serde_json::from_str(&folders).unwrap();
        assert_eq!(f.as_array().unwrap().len(), 1);
        assert_eq!(
            m["folderCount"].as_i64().unwrap(),
            1,
            "folderCount must equal foldersJson.length (D5)"
        );
        assert_eq!(m["playlistCount"].as_i64().unwrap(), 0);
        assert!(m["playlists"].as_array().unwrap().is_empty());
        assert!(m["tree"].as_array().unwrap().is_empty());
        // The defaults QML reads with `!== false`.
        assert_eq!(m["sortAsc"], serde_json::json!(true));
        assert_eq!(m["folderMode"], serde_json::json!(true));
        assert_eq!(m["viewMode"], serde_json::json!("grid"));
        *FOLDERS_CACHE.lock().unwrap() = Vec::new();
        reset_toolbar();
    }

    #[test]
    fn the_tree_is_published_only_in_folder_mode_tree_view() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_toolbar();
        *CACHE.lock().unwrap() = Some(PmData {
            playlists: vec![PmPlaylist {
                id: 1,
                name: "Alpha".into(),
                ..Default::default()
            }],
            folders: vec![],
            locals: vec![],
        });
        let tree_len = || -> usize {
            let (manager, _) = build_document();
            let v: serde_json::Value = serde_json::from_str(&manager).unwrap();
            v["tree"].as_array().unwrap().len()
        };
        assert_eq!(tree_len(), 0, "grid mode publishes no tree");
        set_view_mode("tree");
        assert_eq!(tree_len(), 1);
        toggle_folder_mode();
        assert_eq!(tree_len(), 0, "leaving folder mode empties the tree");
        *CACHE.lock().unwrap() = None;
        reset_toolbar();
    }
}
