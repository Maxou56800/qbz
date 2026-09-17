//! Local Library shared state: the per-user `library.db` handle helper, the
//! persisted toolbar prefs, and the in-process document cache the loaders
//! fill and the playback/artwork paths read back.
//!
//! Split out of `local_library_qt.rs` (phase-24 modularization). ADR-006:
//! nothing here re-implements scanning or grouping — every read goes through
//! `qbz_library::LibraryDatabase`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use qbz_library::{LibraryDatabase, LibraryError, LocalTrack};
use qbz_source::SourceId;

use crate::local_rows::{AlbumRow, ArtistRow, LocalCounts, TrackRow, TreeNode};

/// Tracks-tab page size. The Tracks table is the 16K-row freeze surface, so
/// it stays server-paginated; the other tabs are bounded and full-load.
pub const TRACKS_PAGE: u64 = 500;

/// Independent Phase-A offsets. Phase E replaces them with catalog keysets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrackSourceOffsets {
    pub local: u64,
    pub plex: u64,
    pub jellyfin: u64,
    pub subsonic: u64,
}

impl TrackSourceOffsets {
    pub fn add_assign(&mut self, consumed: Self) {
        self.local += consumed.local;
        self.plex += consumed.plex;
        self.jellyfin += consumed.jellyfin;
        self.subsonic += consumed.subsonic;
    }
}

#[derive(Debug, Clone)]
pub struct TracksLoadRequest {
    pub generation: u64,
    pub offsets: TrackSourceOffsets,
    pub query: String,
    pub sort: String,
    pub group: String,
    pub filter: crate::local_filter::MediaFilter,
}

// ---------------------------------------------------------------------------
// Database access (library_db.rs `with_db` 1:1 — a fresh connection per op on
// the calling BLOCKING thread; `LibraryDatabase` holds a non-Send rusqlite
// handle, so it never crosses an await point).
// ---------------------------------------------------------------------------

/// `unwrap_or(0)` = the GUEST profile — kept identical to
/// `library_db_qt::db_path`, which carries the full reasoning. Short version:
/// `?`-ing out here made the whole local library invisible on a machine that
/// has never completed a Qobuz login, and `users/0/` is the profile
/// `activate_offline` and `adopt_guest_profile` already use for that user.
/// The two helpers open the SAME file and must never disagree about where it
/// is.
pub fn db_path() -> Option<PathBuf> {
    Some(
        crate::local_service_qt::current()?
            .service
            .store()
            .database_path()
            .to_path_buf(),
    )
}

pub fn with_db<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&LibraryDatabase) -> Result<R, LibraryError>,
{
    match qbz_library::LibraryStore::new(db_path()?).read(f) {
        Ok(result) => result,
        Err(e) => {
            log::error!("[qbz-qt] local library operation failed: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct LocalState {
    pub albums: Vec<AlbumRow>,
    pub folders: Vec<AlbumRow>,
    pub artists: Vec<ArtistRow>,
    pub tracks: Vec<TrackRow>,
    /// The RAW rows behind `tracks` (local + Plex). Playback reads these
    /// instead of re-querying by id — a Plex row has no `local_tracks` id to
    /// re-query with, and the local path saves an N-query loop.
    pub tracks_raw: Vec<LocalTrack>,
    /// The RAW rows behind the OPEN detail pane (album or folder) — same
    /// rationale as `tracks_raw`: a context-menu enqueue on a Plex detail row
    /// has no `local_tracks` id to re-query with.
    pub detail_raw: Vec<LocalTrack>,
    /// The currently selected physical version for every album expanded by
    /// Genres Details. Kept separate from the routed album pane so concurrent
    /// expanded albums cannot clobber one another's row actions.
    pub genre_detail_raw: HashMap<String, Vec<LocalTrack>>,
    /// Unfiltered authoritative rows behind the same bounded detail cache.
    /// Source/format/quality chip changes derive a new picker document from
    /// these rows instead of re-querying every enabled backend.
    pub genre_detail_all_tracks: HashMap<String, Arc<Vec<LocalTrack>>>,
    /// Every physical version behind the same expanded albums. The QML
    /// version picker changes `genre_detail_raw` from this cache, without a
    /// DB round-trip and without retargeting another expanded album.
    pub genre_detail_versions: HashMap<String, Arc<Vec<(String, Vec<LocalTrack>)>>>,
    /// Last serialized selected-version document for the same bounded set.
    /// A Genres view destroyed by navigation can republish this immediately
    /// instead of re-querying every authoritative source on Back.
    pub genre_detail_docs: HashMap<String, String>,
    /// Media funnel used to build each cached Genres detail. The album id
    /// alone is insufficient because the same logical row can expose a
    /// different set of physical versions after a source/filter change.
    pub genre_detail_filters: HashMap<String, String>,
    /// Requested (possibly in-flight) funnel per expanded album. This must be
    /// separate from `genre_detail_filters`: marking an old document with the
    /// new filter before its worker finished let a duplicate request publish
    /// that stale document as a cache hit, so the version picker appeared not
    /// to gain a newly enabled source.
    pub genre_detail_requests: HashMap<String, String>,
    /// Last version-selection request for each expanded album. Version
    /// documents are serialized off the UI thread, so a quick A -> B -> A
    /// sequence needs a per-album generation to keep B from publishing last.
    pub genre_detail_version_generations: HashMap<String, u64>,
    pub tracks_offsets: TrackSourceOffsets,
    /// Search/sort generation. An older worker may not mutate or publish.
    pub tracks_generation: u64,
    pub tracks_query: String,
    pub tracks_sort: String,
    /// Tracks-tab grouping ("off" | "album" | "artist" | "name") — persisted
    /// (locallibrary_ui `tracks_group`). It is part of the immutable paged
    /// query: otherwise a later page can sort ahead of the visible prefix.
    pub tracks_group: String,
    pub tracks_filter: String,
    pub tracks_has_more: bool,
    /// The FULL flattened tree (visible derivation applies the rail search).
    pub tree: Vec<TreeNode>,
    pub tree_search: String,
    /// artKey -> `(the source that owns the row, its RAW artwork token)`.
    ///
    /// It used to hold the token alone, and `artwork_qt::classify` sniffed the
    /// provenance back out of the characters at window time — which is bug 3.
    /// It briefly held a fully resolved `ArtRef` instead, which fixed that but
    /// moved the resolution from ~50 VISIBLE rows to all 1703 rows of the
    /// document, and a Plex row's resolution reads its credentials: measured
    /// 39-92 µs/row against 2 µs for a row that needs none.
    ///
    /// Carrying the SOURCE beside the token keeps both properties. Provenance
    /// survives — nobody sniffs anything — and the resolution happens where it
    /// always did, in `local_artwork::resolve_window_blocking`, over the keys
    /// actually on screen.
    pub art_index: HashMap<String, (SourceId, String)>,
    /// Album identity ("folder" | "metadata") — persisted (locallibrary_ui).
    pub album_mode: String,
    /// Library Explorer leading columns ("genre" | "year" | "both") —
    /// persisted beside the other Local Library presentation choices.
    pub explorer_columns: String,
    pub counts: LocalCounts,
    /// Synthetic logical album id -> strongly-associated authoritative album
    /// ids. Rebuilt with each Albums query; never persisted and therefore
    /// reversible when source evidence changes.
    pub album_version_ids: HashMap<String, Vec<String>>,
    /// The OPEN local album's group key ("" when nothing is open).
    pub album_id: String,
    /// The OPEN album's VERSIONS — (source-directory key, its tracks). A
    /// "version" is a distinct PHYSICAL copy; metadata identity can fold
    /// several directories into one card, and merging their tracks would
    /// render a duplicated list. Cached so the picker switches with no DB
    /// round-trip (the Slint's `ALBUM_VERSIONS`). See `local_album_actions`.
    pub album_versions: Vec<(String, Vec<LocalTrack>)>,
    /// Index into `album_versions` currently shown by the picker.
    pub album_version_index: usize,
}

static LOCAL: Mutex<Option<LocalState>> = Mutex::new(None);
// Serializes the take/map/restore artwork transaction without retaining the
// much broader LocalState lock while a large album/artist page is mapped.
// Every production read/write of `art_index` goes through this gate.
static ART_INDEX_GATE: Mutex<()> = Mutex::new(());

struct ArtIndexTransaction {
    art: Option<HashMap<String, (SourceId, String)>>,
}

impl Drop for ArtIndexTransaction {
    fn drop(&mut self) {
        if let Some(art) = self.art.take() {
            state(|state| state.art_index = art);
        }
    }
}

pub fn state<R>(f: impl FnOnce(&mut LocalState) -> R) -> R {
    // A failed assertion in one Rust test must not make every subsequent
    // Local Library operation panic. Production closures are deliberately
    // panic-free, but recovering the owned state is safer than abandoning it.
    let mut guard = LOCAL.lock().unwrap_or_else(|error| error.into_inner());
    let s = guard.get_or_insert_with(|| {
        let prefs = read_prefs();
        LocalState {
            album_mode: prefs.albums_id_mode,
            tracks_sort: prefs.tracks_sort,
            tracks_group: prefs.tracks_group,
            tracks_filter: prefs.tracks_filter,
            explorer_columns: normalize_explorer_columns(&prefs.explorer_columns).to_string(),
            ..LocalState::default()
        }
    });
    f(s)
}

/// Mutate the artwork index as one serialized transaction.
///
/// Mapping runs on several native paging workers at once (Artists alone has
/// an artist rail and an album grid).  Taking the map out and restoring it in
/// two separate critical sections lets two workers both observe an empty map;
/// whichever one restores last then discards every artwork reference the
/// other worker registered. The dedicated gate makes that transaction atomic
/// while releasing the broader LocalState lock during potentially large pure
/// mapping work, so artwork correctness does not stall unrelated UI reads.
pub fn with_art<R>(f: impl FnOnce(&mut HashMap<String, (SourceId, String)>) -> R) -> R {
    let _gate = ART_INDEX_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut transaction = ArtIndexTransaction {
        art: Some(state(|state| std::mem::take(&mut state.art_index))),
    };
    // `ArtIndexTransaction::drop` restores the map on both the normal and
    // panic paths, before `_gate` is released.
    f(transaction.art.as_mut().expect("art transaction is active"))
}

/// Snapshot the requested artwork references while respecting an in-flight
/// mapping transaction. Readers must never observe the deliberate empty map
/// between `take` and restore.
pub fn artwork_sources(keys: &[String]) -> Vec<(String, SourceId, String)> {
    let _gate = ART_INDEX_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    state(|state| {
        keys.iter()
            .filter_map(|key| {
                state
                    .art_index
                    .get(key)
                    .map(|(source, token)| (key.clone(), *source, token.clone()))
            })
            .collect()
    })
}

/// Drop every cached document (logout / user switch).
pub fn reset() {
    let _gate = ART_INDEX_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    *LOCAL.lock().unwrap() = None;
}

// ---------------------------------------------------------------------------
// Toolbar prefs (locallibrary_prefs.rs 1:1 — the SAME json the Slint writes,
// so the two frontends share the user's album-identity + sort choices).
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Prefs {
    #[serde(default = "d_off")]
    pub tracks_group: String,
    #[serde(default = "d_default")]
    pub tracks_sort: String,
    #[serde(default)]
    pub tracks_filter: String,
    #[serde(default = "d_folder")]
    pub albums_id_mode: String,
    #[serde(default = "d_genre")]
    pub explorer_columns: String,
    #[serde(default)]
    pub ephemeral_folder: Option<String>,
}

fn d_off() -> String {
    "off".into()
}
fn d_default() -> String {
    "default".into()
}
fn d_folder() -> String {
    "folder".into()
}
fn d_genre() -> String {
    "genre".into()
}

fn normalize_explorer_columns(value: &str) -> &'static str {
    match value {
        "year" => "year",
        "both" => "both",
        _ => "genre",
    }
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            tracks_group: d_off(),
            tracks_sort: d_default(),
            tracks_filter: String::new(),
            albums_id_mode: d_folder(),
            explorer_columns: d_genre(),
            ephemeral_folder: None,
        }
    }
}

fn prefs_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("qbz").join("locallibrary_ui.json"))
}

pub fn read_prefs() -> Prefs {
    let Some(path) = prefs_path() else {
        return Prefs::default();
    };
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Prefs::default(),
    }
}

/// Merge this struct's owned keys into the on-disk document and publish it
/// ATOMICALLY (`settings_qt::write_json_object_atomic`).
///
/// Two changes from the old whole-struct `std::fs::write`, both because this
/// file is co-owned with the SHIPPING Slint build (`locallibrary_prefs.rs`,
/// same path):
///
/// 1. `fs::write` opens O_TRUNC, so between the truncate and the last byte the
///    file is short or empty. Slint's reader answers a parse failure with
///    `Prefs::default()` and writes those defaults back on its next toolbar
///    change — one unlucky read inside our window resets the user's album
///    identity and sort. The temp-file + `rename(2)` publish closes it: a
///    concurrent reader sees the whole old document or the whole new one.
/// 2. Serializing the struct REPLACED the document, so any key Slint gains
///    later would be dropped by this frontend on the first toolbar click.
///    Merging keeps unknown keys, and `read_json_object` refuses to write at
///    all when the document did not parse rather than rebuilding it.
pub fn write_prefs(p: &Prefs) {
    let Some(path) = prefs_path() else {
        return;
    };
    let Some(mut doc) = crate::settings_qt::read_json_object(&path) else {
        return;
    };
    merge_prefs(&mut doc, p);
    crate::settings_qt::write_json_object_atomic(&path, &doc);
}

fn merge_prefs(doc: &mut serde_json::Map<String, serde_json::Value>, p: &Prefs) {
    if let Ok(serde_json::Value::Object(map)) = serde_json::to_value(p) {
        for (key, value) in map {
            doc.insert(key, value);
        }
    }
}

/// Read-modify-write of ONE document: `edit` sees the prefs as they are on
/// disk RIGHT NOW and the result goes back into the same map.
///
/// The `read_prefs()` -> mutate -> `write_prefs()` shape it replaces has the
/// torn-read hole: a read that lands inside another process's write window
/// returns `Prefs::default()`, and the write that follows commits those
/// defaults over the keys the caller never meant to touch.
fn update_prefs(edit: impl FnOnce(&mut Prefs)) {
    let Some(path) = prefs_path() else {
        return;
    };
    let Some(mut doc) = crate::settings_qt::read_json_object(&path) else {
        return;
    };
    // A valid JSON object whose OWN keys are the wrong shape: repair to the
    // defaults (what every reader on both sides already sees) but say so —
    // this is the one path here that overwrites values it could not read.
    let mut prefs: Prefs = serde_json::from_value(serde_json::Value::Object(doc.clone()))
        .unwrap_or_else(|e| {
            log::warn!("[qbz-qt] locallibrary_ui.json keys unreadable ({e}) — using defaults");
            Prefs::default()
        });
    edit(&mut prefs);
    merge_prefs(&mut doc, &prefs);
    crate::settings_qt::write_json_object_atomic(&path, &doc);
}

// ---------------------------------------------------------------------------
// Toolbar setters / getters (all cheap; the loads that follow are queued by
// the bridge)
// ---------------------------------------------------------------------------

pub fn group_mode() -> qbz_library::album_grouping::AlbumGroupMode {
    qbz_library::album_grouping::AlbumGroupMode::from_pref(&state(|s| s.album_mode.clone()))
}

pub fn set_album_mode(mode: &str) {
    let mode = if mode == "metadata" {
        "metadata"
    } else {
        "folder"
    };
    state(|s| s.album_mode = mode.to_string());
    update_prefs(|p| p.albums_id_mode = mode.to_string());
    // The grouping IS the query: `LocalSource` resolves an album's tracks
    // through `get_album_tracks_metadata` or `get_album_tracks` depending on
    // it. Leaving the source on its default would make album PLAYBACK use a
    // different track list than the grid just rendered.
    crate::source_wiring::sync_album_group_mode();
}

pub fn album_mode() -> String {
    state(|s| s.album_mode.clone())
}

pub fn set_explorer_columns(mode: &str) {
    let mode = normalize_explorer_columns(mode);
    state(|state| state.explorer_columns = mode.to_string());
    update_prefs(|prefs| prefs.explorer_columns = mode.to_string());
}

pub fn explorer_columns() -> String {
    state(|state| state.explorer_columns.clone())
}

pub fn set_tracks_sort(sort: &str) {
    state(|s| s.tracks_sort = sort.to_string());
    update_prefs(|p| p.tracks_sort = sort.to_string());
}

pub fn tracks_sort() -> String {
    state(|s| s.tracks_sort.clone())
}

/// Tracks-tab grouping. Persisted through the SAME read-modify-write of
/// `locallibrary_ui.json` the sort and the album identity use — the key was
/// already read back on load and preserved on write, it was only ever missing
/// its setter and its seed (PARITY-DEBT #13).
pub fn set_tracks_group(mode: &str) {
    let mode = match mode {
        "album" | "artist" | "name" => mode,
        _ => "off",
    };
    state(|s| s.tracks_group = mode.to_string());
    update_prefs(|p| p.tracks_group = mode.to_string());
}

pub fn tracks_group() -> String {
    state(|s| s.tracks_group.clone())
}

pub fn set_tracks_query(q: &str) {
    state(|s| s.tracks_query = q.to_string());
}

pub fn tracks_query() -> String {
    state(|s| s.tracks_query.clone())
}

pub fn set_tracks_filter(json: &str) {
    state(|s| s.tracks_filter = json.to_string());
    update_prefs(|p| p.tracks_filter = json.to_string());
}

pub fn tracks_filter() -> String {
    state(|s| s.tracks_filter.clone())
}

pub fn begin_tracks_load(reset: bool) -> TracksLoadRequest {
    state(|s| {
        if reset {
            s.tracks.clear();
            s.tracks_raw.clear();
            s.tracks_offsets = TrackSourceOffsets::default();
            s.tracks_has_more = false;
            s.tracks_generation = s.tracks_generation.wrapping_add(1);
            if s.tracks_generation == 0 {
                s.tracks_generation = 1;
            }
        }
        TracksLoadRequest {
            generation: s.tracks_generation,
            offsets: s.tracks_offsets,
            query: s.tracks_query.clone(),
            sort: s.tracks_sort.clone(),
            group: s.tracks_group.clone(),
            filter: crate::local_filter::MediaFilter::from_json(&s.tracks_filter),
        }
    })
}

pub fn tracks_generation() -> u64 {
    state(|s| s.tracks_generation)
}

pub(crate) fn tracks_request_is_current(
    current_generation: u64,
    current_offsets: TrackSourceOffsets,
    request: &TracksLoadRequest,
) -> bool {
    current_generation == request.generation && current_offsets == request.offsets
}

pub fn commit_tracks_page(
    request: &TracksLoadRequest,
    rows: Vec<TrackRow>,
    raw: Vec<LocalTrack>,
    art: HashMap<String, (SourceId, String)>,
    consumed: TrackSourceOffsets,
    has_more: bool,
) -> Option<Vec<TrackRow>> {
    let _gate = ART_INDEX_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    state(|s| {
        if !tracks_request_is_current(s.tracks_generation, s.tracks_offsets, request) {
            return None;
        }
        s.tracks_offsets.add_assign(consumed);
        s.art_index.extend(art);
        s.tracks.extend(rows);
        s.tracks_raw.extend(raw);
        s.tracks_has_more = has_more;
        Some(s.tracks.clone())
    })
}

pub fn tracks_has_more() -> bool {
    state(|s| s.tracks_has_more)
}

pub fn counts() -> LocalCounts {
    state(|s| s.counts.clone())
}

/// Whether any registered folder or indexed track makes the library usable.
/// Offline downloads are indexed without registering a music folder.
/// A server-only setup is ALSO usable — the browse union has content even
/// with no registered on-disk folder.
pub fn has_library() -> bool {
    let has_folders = with_db(|db| db.get_folders())
        .map(|f| !f.is_empty())
        .unwrap_or(false);
    let indexed_tracks = with_db(|db| db.count_all_local_tracks()).unwrap_or(0);
    library_sources_available(
        has_folders,
        indexed_tracks,
        crate::local_plex::is_configured(),
        crate::local_plex::cached_track_count(),
        crate::media_servers_qt::cached_track_count(),
    )
}

fn library_sources_available(
    has_folders: bool,
    indexed_tracks: u64,
    plex_enabled: bool,
    plex_tracks: i64,
    remote_tracks: i64,
) -> bool {
    has_folders || indexed_tracks > 0 || (plex_enabled && plex_tracks > 0) || remote_tracks > 0
}

#[cfg(test)]
mod phase_a_tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    use qbz_source::SourceId;

    use super::{
        library_sources_available, normalize_explorer_columns, tracks_request_is_current, with_art,
        Prefs, TrackSourceOffsets, TracksLoadRequest,
    };

    #[test]
    fn artwork_index_mutations_are_one_atomic_critical_section() {
        let first_key = "test:atomic-art:first".to_string();
        let second_key = "test:atomic-art:second".to_string();
        with_art(|art| {
            art.remove(&first_key);
            art.remove(&second_key);
        });

        let (first_entered_tx, first_entered_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let first = first_key.clone();
        let first_worker = std::thread::spawn(move || {
            with_art(|art| {
                first_entered_tx.send(()).unwrap();
                release_first_rx.recv().unwrap();
                art.insert(first, (SourceId::LOCAL, "first".to_string()));
            });
        });
        first_entered_rx.recv().unwrap();

        let (second_entered_tx, second_entered_rx) = mpsc::channel();
        let second = second_key.clone();
        let second_worker = std::thread::spawn(move || {
            with_art(|art| {
                second_entered_tx.send(()).unwrap();
                art.insert(second, (SourceId::LOCAL, "second".to_string()));
            });
        });

        assert!(matches!(
            second_entered_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        release_first_tx.send(()).unwrap();
        first_worker.join().unwrap();
        second_entered_rx.recv().unwrap();
        second_worker.join().unwrap();

        with_art(|art| {
            assert_eq!(art.get(&first_key).unwrap().1, "first");
            assert_eq!(art.get(&second_key).unwrap().1, "second");
            art.remove(&first_key);
            art.remove(&second_key);
        });
    }

    #[test]
    fn remote_only_installation_has_a_library() {
        assert!(library_sources_available(true, 0, false, 0, 0));
        assert!(library_sources_available(false, 0, true, 5_137, 0));
        assert!(library_sources_available(false, 0, false, 0, 4_924));
        assert!(library_sources_available(false, 0, false, 0, 6_678));
        assert!(!library_sources_available(false, 0, false, 0, 0));
    }

    #[test]
    fn offline_only_installation_has_a_library_without_folders_or_servers() {
        // #758: 7 albums / 97 indexed downloaded tracks, no music folders.
        assert!(library_sources_available(false, 97, false, 0, 0));
        assert!(library_sources_available(false, 1, false, 0, 0));
        // Removing the last download restores the genuine empty state.
        assert!(!library_sources_available(false, 0, false, 0, 0));
        assert!(!library_sources_available(false, 0, false, 20, 0));
    }

    #[test]
    fn an_older_query_finishing_after_a_new_one_is_rejected() {
        let generation = Arc::new(AtomicU64::new(1));
        let (old_started_tx, old_started_rx) = mpsc::channel();
        let (allow_old_finish_tx, allow_old_finish_rx) = mpsc::channel();
        let old_generation = Arc::clone(&generation);
        let old_request = TracksLoadRequest {
            generation: 1,
            offsets: TrackSourceOffsets::default(),
            query: "old".to_string(),
            sort: "title-asc".to_string(),
            group: "off".to_string(),
            filter: Default::default(),
        };
        let old = std::thread::spawn(move || {
            old_started_tx.send(()).unwrap();
            allow_old_finish_rx.recv().unwrap();
            tracks_request_is_current(
                old_generation.load(Ordering::SeqCst),
                TrackSourceOffsets::default(),
                &old_request,
            )
        });

        old_started_rx.recv().unwrap();
        generation.store(2, Ordering::SeqCst);
        let new_request = TracksLoadRequest {
            generation: 2,
            offsets: TrackSourceOffsets::default(),
            query: "new".to_string(),
            sort: "artist-asc".to_string(),
            group: "off".to_string(),
            filter: Default::default(),
        };
        allow_old_finish_tx.send(()).unwrap();

        assert!(!old.join().unwrap());
        assert!(tracks_request_is_current(
            generation.load(Ordering::SeqCst),
            TrackSourceOffsets::default(),
            &new_request,
        ));
    }

    #[test]
    fn tracks_tab_keeps_an_origin_aware_page_trigger_and_manual_fallback() {
        // Smoke regression (2026-08-23): the compatibility ListView reached
        // exactly three 500-row pages and then stranded the remaining 27,871
        // rows. ListView's visual offset is contentY - originY after repeated
        // model replacement; a raw contentY comparison is not stable. Keep a
        // second, explicit affordance so an omitted Flickable terminal signal
        // can never make the catalog unreachable again.
        let qml = include_str!("../qml/views/local/LocalTracksTab.qml");
        assert!(qml.contains("list.contentY - list.originY"));
        assert!(qml.contains("onAtYEndChanged"));
        assert!(qml.contains("onMovementEnded"));
        assert!(qml.contains("QbzLoadMore"));
        assert!(qml.contains("onClicked: root.requestNextPage(true)"));
    }

    #[test]
    fn tracks_page_republish_keeps_the_visible_track_anchor() {
        // A legacy page append republishes the accumulated rows as JSON. Keep
        // one ListModel mounted and append an immutable prefix in-place so
        // QQuickItemView never receives setModel(). Full-query replacement
        // still follows track identity and retains the clipped-row offset.
        let qml = include_str!("../qml/views/local/LocalTracksTab.qml");
        let view_qml = include_str!("../qml/views/LocalLibraryView.qml");
        assert!(qml.contains("id: legacyEntriesModel"));
        assert!(!qml.contains("dynamicRoles: true"));
        assert!(qml.contains("function canAppendLegacyEntries(nextEntries)"));
        assert!(qml.contains("legacyEntriesModel.append({\"modelData\": nextEntries[i]})"));
        assert!(qml.contains("? root.view.nativeTracksModel : legacyEntriesModel"));
        assert!(!qml.contains("? root.view.nativeTracksModel : root.entries"));
        assert!(qml.contains("function capturePageAnchor()"));
        assert!(qml.contains("String(root.entries[i].row.id)"));
        assert!(qml.contains("cell.y - list.contentY"));
        assert!(!qml.contains("cell.mapToItem(list"));
        assert!(qml.contains("|| root.restoringPageAnchor"));
        assert!(qml.contains("root.pendingPageAnchor = root.capturePageAnchor()"));
        assert!(qml.contains("var anchor = root.pendingPageAnchor"));
        assert!(qml.contains("id: pageAnchorSettle"));
        assert!(qml.contains("id: pageRequestRelease"));
        assert!(qml.contains("pageAnchorSettle.restart()"));
        assert!(qml.contains("var wanted = list.contentY + currentScreenY - anchor.screenY"));
        assert!(qml.contains("function restorePageAnchor(anchor, nextEntries, epoch)"));
        assert!(qml.contains("Qt.callLater(function ()"));
        assert!(qml.contains("list.positionViewAtIndex(target, ListView.Beginning)"));
        assert!(qml.contains("root.publishLegacyEntries(out, anchor, epoch)"));
        assert!(qml.contains("root.restorePageAnchor(anchor, nextEntries, epoch)"));
        assert!(view_qml.contains("readonly property var tracksVisible: tracks"));
        assert!(!view_qml.contains("readonly property var tracksVisible: {"));
    }

    #[test]
    fn recycled_card_binding_repairs_are_bound_to_delegate_lifetimes() {
        let local = include_str!("../qml/views/local/LocalAlbumCollection.qml");
        let feed = include_str!("../qml/views/library/FeedGridCell.qml");
        assert!(local.contains("id: mutableRestore"));
        assert!(local.contains("mutableRestore.restart()"));
        assert!(!local.contains(
            "Qt.callLater(function () {\n                                    if (cardCell.slot"
        ));
        assert!(feed.contains("id: mutableRestore"));
        assert!(feed.contains("mutableRestore.restart()"));
        assert!(!feed.contains("Qt.callLater(function () {\n            if (cell.identity"));
    }

    #[test]
    fn genres_details_virtualizes_the_nested_box_set_rows() {
        // The outer ListView is album-granular: a 150-track box is one visible
        // delegate. A plain nested Repeater therefore creates every wrapper and
        // Loader before the first frame. Keep the album's exact variable-height
        // geometry, but move a non-interactive inner ListView through the visible
        // slice so Qt constructs only the rows in its look-ahead band. Menus are
        // click-cold for the same reason.
        let details = include_str!("../qml/views/local/LocalGenreDetails.qml");
        let column = include_str!("../qml/views/local/LocalGenreColumn.qml");
        let tab = include_str!("../qml/views/local/LocalGenresTab.qml");
        let versions = include_str!("../qml/views/local/VersionPicker.qml");
        let album_row = include_str!("../qml/views/local/LocalAlbumRow.qml");
        assert!(details.contains("readonly property real trackViewportStart:"));
        assert!(details.contains("readonly property real trackViewportHeight:"));
        assert!(details.contains("id: trackList"));
        assert!(details.contains("contentY: albumBlock.trackViewportStart"));
        assert!(details.contains("interactive: false"));
        assert!(details.contains("reuseItems: true"));
        assert!(!details.contains("readonly property bool inViewportBand:"));
        assert!(!details.contains("visible: trackBlock.showDisc"));
        // A cold album is one compact loading row. `trackCount` is metadata,
        // never permission to reserve hundreds or thousands of blank pixels
        // before the detail query has returned.
        assert!(details.contains("readonly property int loadingBodyH: 50"));
        assert!(details.contains("? (loaded"));
        assert!(details.contains(": loadingBodyH)"));
        // Cache-hit publication is deferred by the Rust bridge, so delegates
        // can request synchronously. All remaining delayed work belongs to the
        // view: those timers are cancelled when its Loader is destroyed and
        // cannot retain a recycled delegate's QML context.
        assert!(details.contains("function ensureCurrent()"));
        assert!(details.contains("Component.onCompleted: ensureCurrent()"));
        assert!(details.contains("id: ensureVisibleTimer"));
        assert!(details.contains("id: reportTimer"));
        assert!(details.contains("interval: 16"));
        assert!(!details.contains("Qt.callLater"));
        assert!(column.contains("interval: root.debounceMs"));
        assert!(tab.contains("root.facetKind(facetIndex) === \"album\" ? 140 : 90"));
        assert!(tab.contains("[\"genre\", \"year\", \"artist\", \"album\"]"));
        assert!(tab.contains("root.view.selectedGenreYears = ({})"));
        assert!(details.contains("maxConcurrentDetailRequests: 2"));
        assert!(details.contains("maxConcurrentDetailRequests - currentPendingCount()"));
        assert!(details.contains("property var detailFilters: ({})"));
        assert!(details.contains("function stableFilterJson(value)"));
        assert!(details.contains("detailFilters[albumId] === mediaFilterJson"));
        assert!(details.contains("previousVersion.key === (nextVersion.key || \"\")"));
        assert!(!details.contains("detailCache = ({})"));
        let bridge = include_str!("local_bridge.rs");
        assert!(bridge.contains("state.genre_detail_requests.get(&id) == Some(&filter_json)"));
        assert!(bridge.contains("state.genre_detail_requests.remove(&request_id)"));
        let bridge = include_str!("local_bridge.rs");
        let cache_path = bridge
            .split("pub fn genre_album_tracks")
            .nth(1)
            .and_then(|tail| tail.split("crate::spawn").next())
            .expect("genre album cache-hit path");
        assert!(cache_path
            .contains("queue_genre_album_ready(id, json, filter_json, \"cache-hit\", started)"));
        assert!(!cache_path.contains("local_genre_album_ready"));
        assert!(
            details.contains("id: discMenuLoader\n                                active: false")
        );
        assert!(versions.contains("id: versionMenuLoader\n        active: false"));
        assert!(album_row.contains("id: rowMenuLoader\n        active: false"));
    }

    #[test]
    fn dsd_filter_prunes_explorer_parent_rows_and_remains_visible_when_collapsed() {
        let view = include_str!("../qml/views/LocalLibraryView.qml");
        let explorer = include_str!("../qml/views/local/LocalGenresTab.qml");

        // Both parent projections use this gate: albums in Library Explorer
        // and artists in the Artists tab. If DSD is omitted here, their tier
        // comparison never runs even though the track/detail backend filters.
        assert_eq!(
            view.matches(
                "var qAny = selected.dsd || selected.hires || selected.cd || selected.lossy",
            )
            .count(),
            2
        );
        assert!(view.contains("(selected.dsd && tier === \"dsd\")"));
        assert!(view.contains("(selected.dsd && value === \"dsd\")"));
        assert!(view.contains("var variants = r.mediaVariants || []"));
        assert!(view.contains("if (qok && fok && sok)"));

        // Collapsing the facet browser must not make the active DSD filter
        // invisible or impossible to clear from its applied-filter strip.
        assert!(explorer.contains("{ key: \"dsd\", label: \"DSD\" }"));
    }

    #[test]
    fn explicit_ephemeral_open_routes_globally_and_keeps_a_loading_surface() {
        let router = include_str!("../qml/shell/ContentRouter.qml");
        let pane = include_str!("../qml/views/local/LocalEphemeralPane.qml");
        assert!(router.contains("function onLocalEphemeralOpenSeqChanged()"));
        assert!(router.contains("QbzShell.navigateToTab(\"local\", \"ephemeral\")"));
        assert!(pane.contains("visible: QbzLocal.localEphemeralLoading"));
        assert!(pane.contains("variant: \"rowList\""));
    }

    #[test]
    fn explicit_remote_resync_is_authoritative() {
        // Jellyfin deltas cannot enumerate deleted/reminted provider ids. The
        // Local Library menu says "Resync", so both its direct rows and the
        // aggregate action must request a full observation before pruning.
        let chrome = include_str!("../qml/views/local/LocalChrome.qml");
        assert!(chrome.contains("QbzLocal.mediaSync(\"jellyfin\", true)"));
        assert!(chrome.contains("QbzLocal.mediaSync(\"subsonic\", true)"));
        assert!(!chrome.contains("QbzLocal.mediaSync(\"jellyfin\", false)"));
        assert!(!chrome.contains("QbzLocal.mediaSync(\"subsonic\", false)"));
    }

    #[test]
    fn genre_columns_use_the_shared_app_radius() {
        let column = include_str!("../qml/views/local/LocalGenreColumn.qml");
        assert!(column.contains("radius: theme.radiusSm"));
        assert!(column.contains("clip: true"));
    }

    #[test]
    fn track_group_indices_are_complete_and_do_not_depend_on_delegates() {
        let local = include_str!("../qml/views/local/LocalTracksTab.qml");
        let library = include_str!("../qml/views/LibraryView.qml");
        let strip = include_str!("../qml/controls/QbzAlphaStrip.qml");
        assert!(local.contains("visible: root.view.tracksGroup !== \"off\""));
        assert!(local.contains("completeAlphabet: true"));
        assert!(library.contains("activeTab === \"tracks\" && tracksGroup !== \"off\""));
        assert!(library.contains("completeAlphabet: !root.dateGrouping"));
        assert!(strip.contains("model: root.completeAlphabet ? 27"));
        assert!(strip.contains("\"#ABCDEFGHIJKLMNOPQRSTUVWXYZ\".charAt(position)"));
        assert!(strip.contains("enabled: cell.entry.index >= 0"));
    }

    #[test]
    fn local_header_scan_progress_uses_bounded_counters_only() {
        let chrome = include_str!("../qml/views/local/LocalChrome.qml");
        let progress = include_str!("../qml/views/local/LocalScanProgress.qml");
        assert!(chrome.contains("LocalScanProgress"));
        assert!(progress.contains("localScan.sourceProcessed"));
        assert!(progress.contains("catalog.overallDone"));
        assert!(!progress.contains("QbzLocalTracks"));
        assert!(!progress.contains("ListView"));
        assert!(!progress.contains("Repeater"));
    }

    #[test]
    fn explorer_column_preference_is_persisted_and_normalized() {
        assert_eq!(normalize_explorer_columns("genre"), "genre");
        assert_eq!(normalize_explorer_columns("year"), "year");
        assert_eq!(normalize_explorer_columns("both"), "both");
        assert_eq!(normalize_explorer_columns("future-value"), "genre");
        let json = serde_json::to_value(Prefs::default()).expect("serializes prefs");
        assert_eq!(json["explorer_columns"], "genre");
    }
}
