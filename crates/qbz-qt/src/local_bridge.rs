//! QbzLocal — the Local Library domain bridge (the phase-23 per-domain
//! pattern, modelled on `home_bridge.rs`: ONE `#[cxx_qt::bridge]` mod, ONE
//! `#[qml_element] #[qml_singleton]` QObject, its own
//! `OnceLock<CxxQtThread>`, a `boot()` invokable and a `pub(crate) fn ui()`).
//!
//! Every property is ONE JSON document (the `library_qt.rs` transport
//! rationale): the QML view parses it once per publish and derives its
//! search / sort / grouping in JS. Artwork never rides the document — it is
//! id-keyed through `localArtworkReady`, windowed by the grid/list, and
//! evicted QML-side, which is what keeps a 16K-track library from decoding
//! covers it never shows.
//!
//! The invokables stay one-line forwards into the `local_*` modules; the
//! blocking DB work runs on `spawn_blocking` so the Qt event loop is never
//! touched by a rusqlite call, and the Plex network work runs on the tokio
//! runtime.

use std::pin::Pin;
use std::sync::OnceLock;

use cxx_qt::CxxQtThread;
use cxx_qt::Threading as _;
use cxx_qt_lib::QString;

use crate::local_bridge_ops::{
    emit_artwork, emit_artwork_one, invalidate_artists, load_tab_impl, load_tracks,
    publish_availability, publish_plex_state, publish_tree, reload_browse, run_sync,
};
use crate::local_library_qt as lib;
use crate::local_plex as plex;

#[cxx_qt::bridge]
pub mod qbz_local {
    extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        // --- Availability / chrome ----------------------------------------
        /// False when there is no per-user library.db, no registered folder
        /// AND no cached Plex content — the view then shows the "nothing
        /// indexed yet" state with the route into Settings > Local Library.
        #[qproperty(bool, local_available)]
        /// {albums, artists, folders, tracks, plexTracks} — the tab badges.
        #[qproperty(QString, local_counts_json)]
        /// Derived-catalog build/reconciliation progress. JSON:
        /// `{active,phase,source,sourceInstance,sourceDone,sourceTotal,
        ///   overallDone,overallTotal,sourceIndex,sourceCount}`.
        /// Counters only: observing this never materializes paged rows.
        #[qproperty(QString, local_catalog_progress_json)]
        /// Album identity: "folder" | "metadata" (persisted; shared with
        /// the Slint frontend's locallibrary_ui.json).
        #[qproperty(QString, local_album_mode)]
        /// Library Explorer columns: "genre" | "year" | "both". Persisted
        /// in locallibrary_ui.json and seeded before the view mounts.
        #[qproperty(QString, local_explorer_columns)]
        // --- Albums tab ---------------------------------------------------
        #[qproperty(bool, local_albums_loading)]
        #[qproperty(QString, local_albums_error)]
        #[qproperty(QString, local_albums_json)]
        /// Phase-F1 surface switch. False keeps the legacy JSON collection;
        /// true binds LocalAlbumsTab to the paged QAbstractListModel.
        #[qproperty(bool, local_albums_native_active)]
        #[qproperty(i64, local_albums_native_total)]
        #[qproperty(i64, local_albums_native_selected_count)]
        /// Bounded A-Z jump metadata (`[{letter,index}]`), never album rows.
        #[qproperty(QString, local_albums_native_jumps_json)]
        #[qproperty(QString, local_albums_native_error)]
        // --- Artists tab --------------------------------------------------
        #[qproperty(bool, local_artists_loading)]
        #[qproperty(QString, local_artists_json)]
        /// Phase-F2 surface switch. False keeps the legacy JSON rail/pane.
        #[qproperty(bool, local_artists_native_active)]
        #[qproperty(i64, local_artists_native_total)]
        #[qproperty(i64, local_artist_albums_native_total)]
        #[qproperty(bool, local_artist_albums_loading)]
        #[qproperty(QString, local_artists_native_jumps_json)]
        #[qproperty(QString, local_artists_native_error)]
        // --- Folders tab, FLAT mode ---------------------------------------
        #[qproperty(bool, local_folders_loading)]
        #[qproperty(QString, local_folders_json)]
        // --- Folders tab, TREE mode ---------------------------------------
        #[qproperty(bool, local_tree_loading)]
        /// The FLATTENED, search-filtered visible tree — a plain array the
        /// rail windows with a ListView (never a recursive component).
        #[qproperty(QString, local_tree_json)]
        /// Tracks selected in the tree rail — the bulk bar's counter and its
        /// visibility gate.
        #[qproperty(i32, local_tree_selected_count)]
        #[qproperty(bool, local_detail_loading)]
        /// {path, name, trackCount, subfolders[], tracks[]}
        #[qproperty(QString, local_detail_json)]
        // --- Tracks tab (server-paginated) --------------------------------
        #[qproperty(bool, local_tracks_loading)]
        #[qproperty(bool, local_tracks_loading_more)]
        #[qproperty(bool, local_tracks_has_more)]
        #[qproperty(QString, local_tracks_sort)]
        /// Persisted quality/format/source funnel JSON for the Tracks query.
        #[qproperty(QString, local_tracks_filter)]
        /// Tracks-tab grouping: "off" | "album" | "artist" | "name". A
        /// CLIENT-side visual reorder (unlike the sort, which is the SQL
        /// ORDER BY), but persisted in the SAME locallibrary_ui.json key the
        /// Slint writes — so it survives a restart and both frontends agree.
        #[qproperty(QString, local_tracks_group)]
        #[qproperty(QString, local_tracks_json)]
        /// Phase-E surface switch. False keeps the existing JSON reader;
        /// true binds LocalTracksTab to QbzLocalTracks (QAbstractListModel).
        #[qproperty(bool, local_tracks_native_active)]
        #[qproperty(i64, local_tracks_native_total)]
        #[qproperty(i64, local_tracks_native_selected_count)]
        /// Bounded A-Z jump metadata (`[{letter,index}]`), never track rows.
        #[qproperty(QString, local_tracks_native_jumps_json)]
        #[qproperty(QString, local_tracks_native_error)]
        // --- Physical media info modal -----------------------------------
        /// Local Library media facts, distinct from Qobuz record info.
        #[qproperty(bool, local_media_info_open)]
        #[qproperty(bool, local_media_info_loading)]
        #[qproperty(QString, local_media_info_json)]
        // --- Local album detail (the album pane) ---------------------------
        #[qproperty(bool, local_album_loading)]
        /// {album:{...}, tracks:[...]} — "" while nothing is open.
        #[qproperty(QString, local_album_json)]
        /// Mirror of AppearanceState.local-library-track-artwork (ui_prefs,
        /// default OFF). OFF is the 16k-row freeze guard — keep the default.
        #[qproperty(bool, local_track_artwork)]
        /// Artist NAME route from the routed local album page into the Artists
        /// tab (local/Plex artists carry no catalog id). "" once consumed.
        #[qproperty(QString, local_pending_artist)]
        /// A pending ROUTE into this view: which tab to show, and an optional
        /// query to pre-filter it with. Set by the cortinilla's local "View
        /// more" links, consumed by LocalLibraryView on mount and on change,
        /// then cleared — the property CHANGE is the trigger, so it has to be
        /// released or the same route cannot fire twice in a row. JSON:
        /// {"tab":"albums|artists|tracks","query":"..."}.
        #[qproperty(QString, local_pending_route)]
        // --- Ephemeral folder (an ad-hoc folder outside the index) ---------
        /// An `Open` action is in flight — the chip is busy.
        ///
        /// NOT the same question as `local_ephemeral_loading`, which asks
        /// whether the OPEN SESSION is still filling in. This one covers the
        /// gap BEFORE there is a session at all: spinning a drive up and
        /// reading a TOC takes seconds, and until this existed those seconds
        /// looked exactly like a click that did nothing — so the user clicked
        /// again, onto a drive that was already being read.
        #[qproperty(bool, local_disc_opening)]
        /// A session is open: its OWN tab appears in Local Library.
        #[qproperty(bool, local_ephemeral_active)]
        /// The folder is being scanned (metadata + CUE + artwork).
        #[qproperty(bool, local_ephemeral_loading)]
        /// {name, path, trackCount, multiAlbum, albums:[…]} — "" while closed.
        #[qproperty(QString, local_ephemeral_json)]
        /// The session's DISPLAY NAME — what the tab and the nav flyout call
        /// it. Computed once here rather than derived twice in QML: the view
        /// already parses the document, but `NavFlyout` does not, and a second
        /// derivation is a second thing that can drift.
        ///
        /// The name is the CONTENT, never a verb: "Now Playing" would be false
        /// the moment a folder is open while something else plays.
        #[qproperty(QString, local_ephemeral_label)]
        /// Bumped ONCE per user-initiated open, and never by the boot restore.
        ///
        /// `local_ephemeral_active` cannot carry this: it is already `true`
        /// when you open a SECOND folder over a first, so a handler watching
        /// it never fires and the view stays on whatever tab you were on. A
        /// sequence changes on every open, which is what "take me to what I
        /// just opened" actually needs.
        #[qproperty(i32, local_ephemeral_open_seq)]
        /// The open session is a DISC — a CD in the drive or a disc image.
        /// Distinct from `local_ephemeral_is_cd`, which is narrower (only a
        /// physical CD can be ripped): this gates what applies to any medium
        /// with an IDENTITY, like correcting its metadata. False for an opened
        /// folder, which has no disc to correct.
        #[qproperty(bool, local_session_is_disc)]
        /// The open session came from a physical CD, so it can be RIPPED.
        /// Derived from the rows themselves rather than passed in — a flag
        /// somebody has to remember to set is a flag that eventually is not.
        #[qproperty(bool, local_ephemeral_is_cd)]
        /// The rip WIZARD's document — `{open, album, tracks, destination,
        /// libraryState, …}`. "" only before the first publish.
        ///
        /// The wizard exists because a CD-DA carries no titles, so a wrong
        /// name is the ordinary case and the only cheap moment to fix it is
        /// before the first byte is written (`rip_wizard_qt`).
        #[qproperty(QString, local_rip_plan)]
        /// A rip is running; the pane shows progress instead of the button.
        #[qproperty(bool, local_rip_active)]
        /// "3/7 · 45%" — already formatted, because the number of things that
        /// can disagree about how to format it is otherwise the number of
        /// places that show it.
        #[qproperty(QString, local_rip_progress)]
        /// The whole job, for the progress modal: `{active, album,
        /// destination, index, count, fraction, overall, tracks}`. Rate-limited
        /// to ~10 Hz at the source — the rip callback fires per chunk.
        #[qproperty(QString, local_rip_status)]
        // --- Plex ----------------------------------------------------------
        /// Master toggle (Settings > Local Library > Plex). Drives whether
        /// the browse union includes Plex at all + the source filter chip.
        #[qproperty(bool, plex_enabled)]
        /// enabled + LAN address + resolved base url + token — the Slint's
        /// `plex-available`: gates the header Sync button and every request
        /// that leaves the process.
        #[qproperty(bool, plex_available)]
        /// A manual sync is in flight (`plex-syncing`) — the Sync button's
        /// busy state.
        #[qproperty(bool, plex_syncing)]
        /// Media-server sweep state. TWO properties rather than one, because
        /// the two questions have different answers: `media_syncing` gates the
        /// spinner, and `media_sync_progress` ("1500/4924") is what makes a
        /// 45.8-second Jellyfin sweep legible instead of a frozen button.
        /// Whether each media server is CONFIGURED — what gates its source
        /// chip in the Local Library filter popup. A chip that can never match
        /// anything is a control that teaches the user the filter is broken,
        /// so the chip only exists when its server does.
        #[qproperty(bool, media_has_jellyfin)]
        #[qproperty(bool, media_has_subsonic)]
        /// The Albums funnel, as a JSON object of the ticked keys
        /// (`{"hires":true,"jellyfin":true}`). Empty string = no filter.
        ///
        /// It lives on the BRIDGE, not on `LocalLibraryView`, for the reason
        /// the owner reported: navigating away DESTROYS the view (see
        /// `project_qt_nav_eager_tabs` — Qt, Slint and Tauri all do), so a
        /// view-local `property var filter` cannot survive a round trip to
        /// Discover, let alone a restart. The bridge outlives the view and
        /// mirrors `ui_prefs.json`, so one property buys BOTH kinds of
        /// persistence the owner asked for.
        #[qproperty(QString, albums_filter)]
        #[qproperty(QString, media_status)]
        #[qproperty(bool, media_syncing)]
        #[qproperty(QString, media_sync_progress)]
        /// Tracks written by the last sync; -1 = never synced this session.
        #[qproperty(i32, plex_last_sync_tracks)]
        /// Raw error text from the last Plex operation ("" when fine). QML
        /// wraps it in its own translated frame.
        #[qproperty(QString, plex_error)]
        /// [{key, title, selected}] — the cached library sections + the
        /// user's selection (Settings > Local Library > Plex).
        #[qproperty(QString, plex_sections_json)]
        /// --- PIN sign-in (plex_pin_qt.rs) --------------------------------
        /// The outstanding link code ("" when none). Its presence is what
        /// mounts the "Link code" row, 1:1 with the reference's
        /// `if PlexSettingsState.pin-code != ""`
        /// (LocalLibrarySettings.slint:638).
        #[qproperty(QString, pin_code)]
        /// The plex.tv sign-in url for the "Open Plex sign-in" button.
        #[qproperty(QString, pin_auth_url)]
        /// The pin/start request is in flight ("Working...").
        #[qproperty(bool, pin_busy)]
        type QbzLocal = super::QbzLocalRust;

        /// Registers this object's Qt-thread hop (Main.qml boots EVERY
        /// domain singleton; only QbzSession.boot also fires crate::on_boot).
        #[qinvokable]
        fn boot(self: Pin<&mut QbzLocal>);

        /// Mount / retry for one tab ("albums" | "artists" | "folders" |
        /// "tracks"). Idempotent per publish: the view calls it on mount and
        /// on tab change; a reload is cheap because every tab is one query.
        #[qinvokable]
        fn load_tab(self: Pin<&mut QbzLocal>, tab: QString);

        /// Album identity dropdown ("folder" | "metadata"): persist + reload
        /// the album set (the grouping IS the query).
        #[qinvokable]
        fn set_album_mode(self: Pin<&mut QbzLocal>, mode: QString);

        /// Persist the active Library Explorer column arrangement.
        #[qinvokable]
        fn set_explorer_columns(self: Pin<&mut QbzLocal>, mode: QString);

        // --- Albums tab (native paged model) ------------------------------
        /// Reset the immutable SQL descriptor and publish its first page.
        #[qinvokable]
        fn albums_native_reset(
            self: Pin<&mut QbzLocal>,
            search: QString,
            sort: QString,
            group: QString,
            filter_json: QString,
            columns: i32,
        );
        #[qinvokable]
        fn albums_native_page_miss(self: Pin<&mut QbzLocal>, page: i32, generation: i32);
        #[qinvokable]
        fn albums_native_toggle_select(self: Pin<&mut QbzLocal>, index: i64, shift: bool);
        #[qinvokable]
        fn albums_native_select_all(self: Pin<&mut QbzLocal>);
        #[qinvokable]
        fn albums_native_clear_selection(self: Pin<&mut QbzLocal>);
        #[qinvokable]
        fn albums_native_bulk_action(self: Pin<&mut QbzLocal>, action: QString);

        // --- Artists tab (native paged models) ---------------------------
        #[qinvokable]
        fn artists_native_reset(
            self: Pin<&mut QbzLocal>,
            search: QString,
            sort: QString,
            filter_json: QString,
        );
        #[qinvokable]
        fn artists_native_page_miss(self: Pin<&mut QbzLocal>, page: i32, generation: i32);
        #[qinvokable]
        fn artists_native_select(self: Pin<&mut QbzLocal>, artist: QString, columns: i32);
        #[qinvokable]
        fn artist_albums_native_page_miss(self: Pin<&mut QbzLocal>, page: i32, generation: i32);

        // --- Tracks tab ----------------------------------------------------
        /// Toolbar search (server-side; resets to page 1).
        #[qinvokable]
        fn tracks_search(self: Pin<&mut QbzLocal>, query: QString);
        /// Toolbar sort (SQL ORDER BY; resets to page 1).
        #[qinvokable]
        fn tracks_set_sort(self: Pin<&mut QbzLocal>, sort: QString);
        #[qinvokable]
        fn tracks_set_filter_json(self: Pin<&mut QbzLocal>, json: QString);
        /// Toolbar grouping ("off" | "album" | "artist" | "name"): persist +
        /// republish. NO re-query — the group modes reorder what is already
        /// loaded, on top of the SQL sort.
        #[qinvokable]
        fn tracks_set_group(self: Pin<&mut QbzLocal>, mode: QString);
        /// Infinite scroll: append the next page.
        #[qinvokable]
        fn tracks_load_more(self: Pin<&mut QbzLocal>);
        /// QAbstractListModel page miss -> async catalog keyset page.
        #[qinvokable]
        fn tracks_native_page_miss(self: Pin<&mut QbzLocal>, page: i32, generation: i32);
        #[qinvokable]
        fn tracks_native_toggle_select(self: Pin<&mut QbzLocal>, index: i64, shift: bool);
        #[qinvokable]
        fn tracks_native_select_all(self: Pin<&mut QbzLocal>);
        #[qinvokable]
        fn tracks_native_clear_selection(self: Pin<&mut QbzLocal>);
        /// Index-based actions keep the source-native TrackRef inside Rust;
        /// QML never converts it back to a legacy integer id.
        #[qinvokable]
        fn tracks_native_play(self: Pin<&mut QbzLocal>, index: i64);
        #[qinvokable]
        fn tracks_native_enqueue(self: Pin<&mut QbzLocal>, index: i64, mode: QString);
        #[qinvokable]
        fn tracks_native_row_action(self: Pin<&mut QbzLocal>, index: i64, action: QString);
        #[qinvokable]
        fn tracks_native_bulk_action(self: Pin<&mut QbzLocal>, action: QString);

        // --- Folder tree ---------------------------------------------------
        /// Chevron: expand (lazy one-level fetch) or collapse (pure UI).
        #[qinvokable]
        fn tree_toggle(self: Pin<&mut QbzLocal>, path: QString, expand: bool);
        /// Rail header: collapse every expanded folder.
        #[qinvokable]
        fn tree_collapse_all(self: Pin<&mut QbzLocal>);
        /// Rail search box (filters the visible set, keeps ancestors).
        #[qinvokable]
        fn tree_search(self: Pin<&mut QbzLocal>, query: QString);
        /// Rail header: enter / leave multi-select. Leaving DROPS the selection.
        #[qinvokable]
        fn tree_set_select_mode(self: Pin<&mut QbzLocal>, on: bool);
        /// Folder checkbox: toggle every track under it, recursively.
        #[qinvokable]
        fn tree_toggle_folder_select(self: Pin<&mut QbzLocal>, path: QString);
        /// Track checkbox: toggle one row by file path.
        #[qinvokable]
        fn tree_toggle_track_select(self: Pin<&mut QbzLocal>, path: QString);
        /// Shift-click on a rail checkbox: SELECT every visible row from the
        /// anchor to the clicked one. `nodes_json` =
        /// `[{"path": …, "isFolder": …}]` in rail order.
        #[qinvokable]
        fn tree_select_range(self: Pin<&mut QbzLocal>, nodes_json: QString);
        /// Tree-rail bulk bar.
        #[qinvokable]
        fn folders_bulk_action(self: Pin<&mut QbzLocal>, action: QString);
        /// Albums-grid / Tracks-table bulk bar. scope = "album" | "track".
        #[qinvokable]
        fn bulk_action(
            self: Pin<&mut QbzLocal>,
            scope: QString,
            ids_json: QString,
            action: QString,
        );
        #[qinvokable]
        fn close_media_info(self: Pin<&mut QbzLocal>);
        #[qinvokable]
        fn copy_media_info(self: Pin<&mut QbzLocal>, value: QString);
        /// Open the local media-info modal for a now-playing local track by its
        /// library row id (the NPB info button / song-card title, which have no
        /// Local Library model row to act on).
        #[qinvokable]
        fn open_media_info_for_track(self: Pin<&mut QbzLocal>, id: i64);
        /// Row body: select a folder and load its detail pane.
        #[qinvokable]
        fn select_folder(self: Pin<&mut QbzLocal>, path: QString);

        // --- Album detail ---------------------------------------------------
        /// Album card click: load the local/Plex album detail (header +
        /// tracks). A `plex:<hash>` id is served from the Plex cache.
        #[qinvokable]
        fn open_album(self: Pin<&mut QbzLocal>, id: QString);
        /// Open a logical album while preserving the Local Library funnel at
        /// the click site; filtered-out physical copies stay inaccessible.
        #[qinvokable]
        fn open_album_filtered(self: Pin<&mut QbzLocal>, id: QString, filter_json: QString);
        /// Close the album pane (back to the grid).
        #[qinvokable]
        fn close_album(self: Pin<&mut QbzLocal>);
        // --- Local album actions -------------------------------------------
        /// The view consumed the pending artist-name route.
        #[qinvokable]
        fn clear_pending_artist(self: Pin<&mut QbzLocal>);
        /// The view applied the pending route — release it.
        #[qinvokable]
        fn clear_pending_route(self: Pin<&mut QbzLocal>);

        /// Ask this view to open on a given tab — `{"tab":"ephemeral"}`. The
        /// counterpart of `clear_pending_route`, and the seam the now-playing
        /// song card uses to route a disc back to the pane it came from.
        #[qinvokable]
        fn set_pending_route(self: Pin<&mut QbzLocal>, route_json: QString);
        /// "Go to artist" on a local/Plex album — a NAME route, not an id.
        #[qinvokable]
        fn open_artist_by_name(self: Pin<&mut QbzLocal>, name: QString);
        /// Artists tab, right pane: the ids of the CACHED album rows that
        /// credit `artist`, as a JSON array (PARITY-DEBT #8). SYNCHRONOUS and
        /// cheap — it is a pass over the album document already in memory, and
        /// QML uses it to filter the very array it renders.
        #[qinvokable]
        fn artist_album_ids(self: &QbzLocal, artist: QString) -> QString;
        /// Genres details mode: fetch one logical album's rows without
        /// navigating away. Results are id-keyed so concurrent visible albums
        /// may finish in any order without publishing over each other.
        #[qinvokable]
        fn genre_album_tracks(self: Pin<&mut QbzLocal>, id: QString, filter_json: QString);
        /// Genres details version picker: switch one expanded album without
        /// retargeting the routed AlbumView or re-querying the database.
        #[qinvokable]
        fn genre_album_select_version(self: Pin<&mut QbzLocal>, album_id: QString, index: i32);
        /// Play/shuffle/enqueue the selected version; `track_id` is empty for
        /// an album header action and set for a displayed track-row click.
        #[qinvokable]
        fn genre_album_action(
            self: Pin<&mut QbzLocal>,
            album_id: QString,
            action: QString,
            track_id: QString,
        );
        /// Play/enqueue the currently selected physical copy in AlbumView.
        #[qinvokable]
        fn album_selected_action(self: Pin<&mut QbzLocal>, action: QString, track_id: QString);
        /// Album header pencil — LOGGED SEAM (no tag-editor modal yet).
        #[qinvokable]
        fn album_edit_tags(self: Pin<&mut QbzLocal>, id: QString);
        /// Album header list-plus — LOGGED SEAM (no playlist picker yet).
        #[qinvokable]
        fn album_add_to_playlist(self: Pin<&mut QbzLocal>, id: QString);
        /// Album header cassette — LOGGED SEAM (no Mixtape store yet).
        #[qinvokable]
        fn album_add_to_mixtape(self: Pin<&mut QbzLocal>, id: QString);
        /// Version picker: switch the shown physical copy.
        #[qinvokable]
        fn album_select_version(self: Pin<&mut QbzLocal>, index: i32);
        /// Per-disc menu: "play" | "next" | "later" | "queue".
        #[qinvokable]
        fn album_disc_action(self: Pin<&mut QbzLocal>, disc: i32, action: QString);
        /// Same menu for one concurrently expanded Genres album. Album id is
        /// required because the routed AlbumView's singleton cache is not the
        /// source of these rows.
        #[qinvokable]
        fn genre_album_disc_action(
            self: Pin<&mut QbzLocal>,
            album_id: QString,
            disc: i32,
            action: QString,
        );
        // --- Ephemeral folder ------------------------------------------------
        /// Pick a folder OUTSIDE the library, scan it, show the pane.
        #[qinvokable]
        fn ephemeral_open(self: Pin<&mut QbzLocal>);
        /// Same, for a KNOWN path (no picker).
        #[qinvokable]
        fn ephemeral_open_path(self: Pin<&mut QbzLocal>, path: QString);
        /// Close the session (stops playback if it came from the session).
        #[qinvokable]
        fn ephemeral_clear(self: Pin<&mut QbzLocal>);
        /// `Open > Open CD…`: read the audio disc in the drive and make it the
        /// ephemeral session. Toasts on every failure it can name (no drive,
        /// no disc, a data-only disc) rather than opening an empty pane.
        #[qinvokable]
        fn ephemeral_open_cd(self: Pin<&mut QbzLocal>);
        /// `Open > Open SACD image…`: pick a .iso and play its stereo area.
        #[qinvokable]
        fn ephemeral_open_sacd(self: Pin<&mut QbzLocal>);
        /// Open the rip wizard for the CD on screen. No-op unless the
        /// session is a physical disc.
        #[qinvokable]
        fn rip_wizard_open(self: Pin<&mut QbzLocal>);
        #[qinvokable]
        fn rip_wizard_close(self: Pin<&mut QbzLocal>);
        /// Ask for the destination and answer the Local Library question.
        #[qinvokable]
        fn rip_pick_destination(self: Pin<&mut QbzLocal>);
        /// Run it, with the form's values.
        #[qinvokable]
        fn rip_start(self: Pin<&mut QbzLocal>, edits_json: QString);
        /// Show or hide the rip PROGRESS panel. Never touches the job.
        #[qinvokable]
        fn rip_panel(self: Pin<&mut QbzLocal>, open: bool);
        /// Stop the running rip after the current chunk. Deletes NOTHING.
        #[qinvokable]
        fn rip_cancel(self: Pin<&mut QbzLocal>);
        /// Header Play / Shuffle: the whole session becomes the queue. Same
        /// `(id, shuffle)` shape as `play_album`, so the two headers behave
        /// identically.
        #[qinvokable]
        fn ephemeral_play_all(self: Pin<&mut QbzLocal>, shuffle: bool);
        /// Per-album Play (multi-album sessions only).
        #[qinvokable]
        fn ephemeral_play_album(self: Pin<&mut QbzLocal>, group_key: QString);
        /// Open the app-wide metadata editor for one physical album directory
        /// in the current ephemeral session.
        #[qinvokable]
        fn ephemeral_edit_tags(self: Pin<&mut QbzLocal>, group_key: QString);
        /// Track row click: the track's album block becomes the queue.
        #[qinvokable]
        fn ephemeral_play_track(self: Pin<&mut QbzLocal>, id: QString);

        // --- Playback --------------------------------------------------------
        /// Album card / detail header Play (optionally shuffled).
        #[qinvokable]
        fn play_album(self: Pin<&mut QbzLocal>, id: QString, shuffle: bool);
        /// Album card Play constrained to the active Local Library funnel.
        #[qinvokable]
        fn play_album_filtered(
            self: Pin<&mut QbzLocal>,
            id: QString,
            filter_json: QString,
            shuffle: bool,
        );
        /// Album-detail row click: play the album from that track.
        #[qinvokable]
        fn play_album_track(self: Pin<&mut QbzLocal>, id: QString, track_id: QString);
        /// Tree detail header Play: the whole subtree becomes the queue.
        #[qinvokable]
        fn play_folder(self: Pin<&mut QbzLocal>, path: QString);
        /// Tree detail row click: the folder's DIRECT tracks become the
        /// queue, starting at the clicked row.
        #[qinvokable]
        fn play_folder_track(self: Pin<&mut QbzLocal>, path: QString, track_id: QString);
        /// Tracks-tab row click: the loaded page set becomes the queue, in the
        /// order the tab is RENDERING it (PARITY-DEBT #14).
        /// `visible_ids_json` = the JSON array of the ids on screen, in render
        /// order; `track_id` = the row that was clicked.
        #[qinvokable]
        fn play_tracks_visible(
            self: Pin<&mut QbzLocal>,
            visible_ids_json: QString,
            track_id: QString,
        );
        /// Context menus: kind = "track" | "album" | "folder";
        /// mode = "next" | "later" | "queue".
        #[qinvokable]
        fn enqueue(self: Pin<&mut QbzLocal>, kind: QString, id: QString, mode: QString);
        /// Album enqueue constrained to the active Local Library funnel.
        #[qinvokable]
        fn enqueue_album_filtered(
            self: Pin<&mut QbzLocal>,
            id: QString,
            filter_json: QString,
            mode: QString,
        );
        /// Heart a genuine local/media-server logical album using the display
        /// snapshot already rendered by the Local Library card.
        #[qinvokable]
        fn album_toggle_favorite(
            self: Pin<&mut QbzLocal>,
            id: QString,
            title: QString,
            artist: QString,
            artwork_url: QString,
            sources_json: QString,
        );

        // --- Plex ------------------------------------------------------------
        /// Header Sync button (#573): re-fetch the Plex sections + tracks
        /// into the shared cache DB, then reload the browse documents in
        /// place. No-op when `plexAvailable` is false.
        /// ── Media servers (Jellyfin / Subsonic) ─────────────────────────
        ///
        /// One set of invokables for both, discriminated by a `server` word
        /// ("jellyfin" | "subsonic"), because the panel is the same form twice
        /// and the store behind it is one table. An unknown word is refused
        /// with a toast rather than silently ignored.
        ///
        /// TEST the address before asking for a password: it separates "wrong
        /// address" from "wrong password", which is the difference between a
        /// user fixing a typo and a user thinking the feature is broken.
        #[qinvokable]
        fn media_test(self: Pin<&mut QbzLocal>, server: QString, url: QString);
        /// Persist the Albums funnel. See the `albums_filter` qproperty.
        #[qinvokable]
        fn set_albums_filter_json(self: Pin<&mut QbzLocal>, json: QString);
        /// Authenticate and persist. Runs a first sweep on success.
        #[qinvokable]
        fn media_connect(
            self: Pin<&mut QbzLocal>,
            server: QString,
            url: QString,
            username: QString,
            password: QString,
        );
        #[qinvokable]
        fn media_quick_connect(self: Pin<&mut QbzLocal>, url: QString);
        #[qinvokable]
        fn media_cancel_quick_connect(self: Pin<&mut QbzLocal>);
        /// Master toggle. OFF collapses the union back immediately; the cache
        /// is KEPT so turning it on again does not re-sweep.
        #[qinvokable]
        fn media_set_enabled(self: Pin<&mut QbzLocal>, server: QString, enabled: bool);
        /// Sweep now. `full` forces a complete pass instead of a delta.
        #[qinvokable]
        fn media_sync(self: Pin<&mut QbzLocal>, server: QString, full: bool);
        /// Sign out: clear credentials and purge this server's cached rows.
        #[qinvokable]
        fn media_disconnect(self: Pin<&mut QbzLocal>, server: QString);
        #[qinvokable]
        fn sync_plex(self: Pin<&mut QbzLocal>);
        /// Master toggle. Turning it OFF collapses the browse union back to
        /// local-only immediately (the cache DB is kept).
        #[qinvokable]
        fn plex_set_enabled(self: Pin<&mut QbzLocal>, enabled: bool);
        /// Manual connect: resolve + persist `proto://host:32400` + token,
        /// then run a first sync. LAN addresses only.
        #[qinvokable]
        fn plex_connect(self: Pin<&mut QbzLocal>, server_url: QString, token: QString);
        /// Sign out of Plex: clear creds/sections and purge the cache DB,
        /// then reload the (now local-only) browse documents.
        #[qinvokable]
        fn plex_disconnect(self: Pin<&mut QbzLocal>);
        /// Persist the selected library section keys (JSON array of strings)
        /// and re-sync so the cache matches the selection.
        #[qinvokable]
        fn set_plex_sections(self: Pin<&mut QbzLocal>, keys_json: QString);
        /// Re-read the Plex gates + sections from the store (call after the
        /// settings panel changed something out of band).
        #[qinvokable]
        fn refresh_plex(self: Pin<&mut QbzLocal>);
        /// Is this address on the local network? A PURE predicate over the
        /// typed text — no state, no IO — so the settings panel can warn and
        /// gate the Connect button WHILE the user types, which is what the
        /// reference does (`PlexSettingsState.is-local-address`, computed in
        /// `plex_auth::refresh_gates` and read at
        /// `LocalLibrarySettings.slint:612` and `:631`).
        #[qinvokable]
        fn plex_url_is_local(self: Pin<&mut QbzLocal>, url: QString) -> bool;

        // --- PIN sign-in (plex_pin_qt.rs) ---------------------------------
        /// "Generate code": ask plex.tv for a PIN and start polling for the
        /// authorization. Takes the address from the panel so a freshly typed
        /// one is used without needing Connect first — the reference does the
        /// same (`plex_auth.rs:415` reads the field, then persists).
        #[qinvokable]
        fn plex_generate_code(self: Pin<&mut QbzLocal>, server_url: QString);
        /// Open the plex.tv sign-in page in the browser.
        #[qinvokable]
        fn plex_open_auth_url(self: Pin<&mut QbzLocal>);
        /// Copy the outstanding link code to the clipboard.
        #[qinvokable]
        fn plex_copy_code(self: Pin<&mut QbzLocal>);
        /// Drop the outstanding PIN and stop polling. The panel calls this on
        /// unmount — the reference has to detect that by watching the
        /// settings section instead, because Slint gives it no unmount hook.
        #[qinvokable]
        fn plex_stop_pin(self: Pin<&mut QbzLocal>);
        /// "Check connection": ping the stored server, report it, and stamp
        /// the machine id onto the cache.
        #[qinvokable]
        fn plex_check_connection(self: Pin<&mut QbzLocal>);

        // --- Windowed artwork -------------------------------------------------
        /// The mounted window reports its artKeys; Rust resolves each to a
        /// 256px thumbnail (local cover) or a disk-cached Plex thumb and
        /// emits `localArtworkReady` per hit.
        #[qinvokable]
        fn artwork_window(self: Pin<&mut QbzLocal>, keys_json: QString);
        #[qinvokable]
        fn artwork_timing_enabled(self: &QbzLocal) -> bool;
        #[qinvokable]
        fn artwork_immediate_enabled(self: &QbzLocal) -> bool;
        #[qinvokable]
        fn artwork_timing(self: &QbzLocal, stage: QString, key: QString, window: QString);
        /// Derive the same flat tint + blurred atmosphere used by Qobuz
        /// AlbumView from an already-resolved local cover. Decode work stays
        /// off the Qt thread; key/path return with the result as generation
        /// guards for QML.
        #[qinvokable]
        fn album_header_atmosphere(self: Pin<&mut QbzLocal>, key: QString, path: QString);
        /// (key, "file://…") — id-keyed so a cover can never land on the
        /// wrong row.
        #[qsignal]
        fn local_artwork_ready(self: Pin<&mut QbzLocal>, key: QString, path: QString);
        #[qsignal]
        fn local_album_atmosphere_ready(
            self: Pin<&mut QbzLocal>,
            key: QString,
            path: QString,
            tint: QString,
            atmosphere: QString,
        );
        #[qsignal]
        fn local_genre_album_ready(
            self: Pin<&mut QbzLocal>,
            id: QString,
            tracks_json: QString,
            filter_json: QString,
        );
        /// Session-only reachability result for one physical local row.
        /// An empty message clears a previous failure after a successful
        /// retry; no database row is rewritten from this transient evidence.
        #[qsignal]
        fn local_track_availability(
            self: Pin<&mut QbzLocal>,
            source: QString,
            id: QString,
            message: QString,
        );
        /// Successful store result (also carries rollback after a failed
        /// optimistic card flip).
        #[qsignal]
        fn local_album_favorite_changed(self: Pin<&mut QbzLocal>, id: QString, favorite: bool);
    }

    impl cxx_qt::Threading for QbzLocal {}
}

use qbz_local::QbzLocal;

/// Rust side of the local-library bridge (plain storage, phase-1 pattern).
pub struct QbzLocalRust {
    local_available: bool,
    local_counts_json: QString,
    local_catalog_progress_json: QString,
    local_album_mode: QString,
    local_explorer_columns: QString,
    local_albums_loading: bool,
    local_albums_error: QString,
    local_albums_json: QString,
    local_albums_native_active: bool,
    local_albums_native_total: i64,
    local_albums_native_selected_count: i64,
    local_albums_native_jumps_json: QString,
    local_albums_native_error: QString,
    local_artists_loading: bool,
    local_artists_json: QString,
    local_artists_native_active: bool,
    local_artists_native_total: i64,
    local_artist_albums_native_total: i64,
    local_artist_albums_loading: bool,
    local_artists_native_jumps_json: QString,
    local_artists_native_error: QString,
    local_folders_loading: bool,
    local_folders_json: QString,
    local_tree_loading: bool,
    local_tree_json: QString,
    local_tree_selected_count: i32,
    local_detail_loading: bool,
    local_detail_json: QString,
    local_tracks_loading: bool,
    local_tracks_loading_more: bool,
    local_tracks_has_more: bool,
    local_tracks_sort: QString,
    local_tracks_filter: QString,
    local_tracks_group: QString,
    local_tracks_json: QString,
    local_tracks_native_active: bool,
    local_tracks_native_total: i64,
    local_tracks_native_selected_count: i64,
    local_tracks_native_jumps_json: QString,
    local_tracks_native_error: QString,
    local_media_info_open: bool,
    local_media_info_loading: bool,
    local_media_info_json: QString,
    local_album_loading: bool,
    local_album_json: QString,
    local_track_artwork: bool,
    local_pending_artist: QString,
    local_pending_route: QString,
    local_disc_opening: bool,
    local_ephemeral_active: bool,
    local_ephemeral_loading: bool,
    local_ephemeral_json: QString,
    local_ephemeral_label: QString,
    local_ephemeral_open_seq: i32,
    local_session_is_disc: bool,
    local_ephemeral_is_cd: bool,
    local_rip_plan: QString,
    local_rip_active: bool,
    local_rip_progress: QString,
    local_rip_status: QString,
    plex_enabled: bool,
    plex_available: bool,
    plex_syncing: bool,
    media_has_jellyfin: bool,
    media_has_subsonic: bool,
    albums_filter: QString,
    media_status: QString,
    media_syncing: bool,
    media_sync_progress: QString,
    plex_last_sync_tracks: i32,
    plex_error: QString,
    plex_sections_json: QString,
    pin_code: QString,
    pin_auth_url: QString,
    pin_busy: bool,
}

impl Default for QbzLocalRust {
    fn default() -> Self {
        Self {
            local_available: false,
            local_counts_json: QString::from("{}"),
            local_catalog_progress_json: QString::from("{\"active\":false}"),
            local_album_mode: QString::from("folder"),
            local_explorer_columns: QString::from("genre"),
            local_albums_loading: false,
            local_albums_error: QString::default(),
            local_albums_json: QString::from("[]"),
            local_albums_native_active: false,
            local_albums_native_total: 0,
            local_albums_native_selected_count: 0,
            local_albums_native_jumps_json: QString::from("[]"),
            local_albums_native_error: QString::default(),
            local_artists_loading: false,
            local_artists_json: QString::from("[]"),
            local_artists_native_active: false,
            local_artists_native_total: 0,
            local_artist_albums_native_total: 0,
            local_artist_albums_loading: false,
            local_artists_native_jumps_json: QString::from("[]"),
            local_artists_native_error: QString::default(),
            local_folders_loading: false,
            local_folders_json: QString::from("[]"),
            local_tree_loading: false,
            local_tree_json: QString::from("[]"),
            local_tree_selected_count: 0,
            local_detail_loading: false,
            local_detail_json: QString::from(""),
            local_tracks_loading: false,
            local_tracks_loading_more: false,
            local_tracks_has_more: false,
            local_tracks_sort: QString::from("default"),
            local_tracks_filter: QString::default(),
            local_tracks_group: QString::from("off"),
            local_tracks_json: QString::from("[]"),
            local_tracks_native_active: false,
            local_tracks_native_total: 0,
            local_tracks_native_selected_count: 0,
            local_tracks_native_jumps_json: QString::from("[]"),
            local_tracks_native_error: QString::default(),
            local_media_info_open: false,
            local_media_info_loading: false,
            local_media_info_json: QString::from("{}"),
            local_album_loading: false,
            local_album_json: QString::from(""),
            local_track_artwork: false,
            local_pending_artist: QString::default(),
            local_pending_route: QString::default(),
            local_disc_opening: false,
            local_ephemeral_active: false,
            local_ephemeral_loading: false,
            local_ephemeral_json: QString::from(""),
            local_ephemeral_label: QString::from(""),
            local_ephemeral_open_seq: 0,
            local_session_is_disc: false,
            local_ephemeral_is_cd: false,
            local_rip_plan: QString::from("{\"open\":false}"),
            local_rip_active: false,
            local_rip_progress: QString::default(),
            local_rip_status: QString::from("{\"active\":false}"),
            plex_enabled: false,
            plex_available: false,
            plex_syncing: false,
            media_has_jellyfin: false,
            media_has_subsonic: false,
            albums_filter: QString::default(),
            media_status: QString::from("{}"),
            media_syncing: false,
            media_sync_progress: QString::default(),
            plex_last_sync_tracks: -1,
            plex_error: QString::default(),
            plex_sections_json: QString::from("[]"),
            pin_code: QString::default(),
            pin_auth_url: QString::default(),
            pin_busy: false,
        }
    }
}

// ---------------------------------------------------------------------------
// The UI hop (bridges/mod.rs pattern)
// ---------------------------------------------------------------------------

static QT_THREAD: OnceLock<CxxQtThread<QbzLocal>> = OnceLock::new();

/// Queue a local-bridge mutation onto the Qt event loop (no-op before boot
/// registers the thread).
pub(crate) fn ui(f: impl FnOnce(Pin<&mut QbzLocal>) + Send + 'static) {
    if let Some(thread) = QT_THREAD.get() {
        let _ = thread.queue(f);
    }
}

/// Publish a transient playback reachability result without changing the
/// authoritative library row. A missing file may have moved and an
/// unreachable NAS may return later; both must remain retryable and neither
/// is evidence that authorizes pruning.
pub(crate) fn emit_track_availability(source: &str, id: u64, message: Option<&str>) {
    let source = source.to_string();
    let id = id.to_string();
    let message = message.unwrap_or_default().to_string();
    ui(move |mut b| {
        b.as_mut().local_track_availability(
            QString::from(source.as_str()),
            QString::from(id.as_str()),
            QString::from(message.as_str()),
        );
    });
}

/// Publish genre details on the next Qt event-loop turn for both cache hits
/// and freshly queried rows. A synchronous cache-hit signal can fire while a
/// Loader is still mounting `LocalGenreDetails`; that first response then has
/// no live `Connections` receiver and its delegate remains pending forever.
fn queue_genre_album_ready(
    id: String,
    json: String,
    filter_json: String,
    phase: &'static str,
    started: std::time::Instant,
) {
    log::info!(
        "[qbz-qt][perf] genre album detail phase={phase} id={id} bytes={} elapsed={:?}",
        json.len(),
        started.elapsed()
    );
    ui(move |mut bridge| {
        bridge.as_mut().local_genre_album_ready(
            QString::from(id.as_str()),
            QString::from(json.as_str()),
            QString::from(filter_json.as_str()),
        );
    });
}

impl qbz_local::QbzLocal {
    pub fn boot(self: Pin<&mut Self>) {
        if QT_THREAD.set(self.qt_thread()).is_err() {
            log::warn!("[qbz-qt] local Qt thread already registered");
        }
        // Seed the persisted toolbar choices so the first paint matches
        // what the user last picked (shared with the Slint frontend).
        // `tracks_group` joined the seed with PARITY-DEBT #13: the key was
        // read and preserved on disk but never reached the UI, so the Tracks
        // tab silently reset to "No grouping" on every launch.
        let mode = lib::album_mode();
        let explorer_columns = lib::explorer_columns();
        let sort = lib::tracks_sort();
        let group = lib::tracks_group();
        let tracks_filter = lib::tracks_filter();
        publish_availability();
        ui(move |mut b| {
            b.as_mut()
                .set_local_album_mode(QString::from(mode.as_str()));
            b.as_mut()
                .set_local_explorer_columns(QString::from(explorer_columns.as_str()));
            b.as_mut()
                .set_local_tracks_sort(QString::from(sort.as_str()));
            b.as_mut()
                .set_local_tracks_group(QString::from(group.as_str()));
            b.as_mut()
                .set_local_tracks_filter(QString::from(tracks_filter.as_str()));
        });
        publish_plex_state();
        crate::local_album_actions::publish_track_artwork();
        // Slint parity: re-open the persisted ad-hoc folder at startup.
        crate::local_ephemeral::rehydrate();
    }

    pub fn load_tab(self: Pin<&mut Self>, tab: QString) {
        load_tab_impl(tab.to_string());
    }

    pub fn set_album_mode(self: Pin<&mut Self>, mode: QString) {
        let mode = mode.to_string();
        lib::set_album_mode(&mode);
        let published = lib::album_mode();
        ui(move |mut b| {
            b.as_mut()
                .set_local_album_mode(QString::from(published.as_str()));
        });
        // The grouping IS the query — reload both album surfaces.
        load_tab_impl("albums".to_string());
        // ...and the ARTISTS tab derives from that same album set: its album
        // cache is keyed by the group key, so a folder-mode compilation
        // cross-lists under every artist until the tab is revisited
        // (PARITY-DEBT #9). The Slint drops the model and lets
        // `ensure_artists_loaded` re-fetch on the next visit
        // (`local_library.rs:727-738 invalidate_artists`); this port has no
        // such guard — `loadTab` re-queries on every tab change — so the
        // equivalent is to drop the cache AND re-run the load right here.
        // That also covers the case the lazy version cannot: the mode is
        // reachable from Settings, so the user can flip it while STANDING on
        // the Artists tab.
        invalidate_artists();
    }

    pub fn set_explorer_columns(self: Pin<&mut Self>, mode: QString) {
        lib::set_explorer_columns(&mode.to_string());
        let published = lib::explorer_columns();
        ui(move |mut bridge| {
            bridge
                .as_mut()
                .set_local_explorer_columns(QString::from(published.as_str()));
        });
    }

    pub fn albums_native_reset(
        self: Pin<&mut Self>,
        search: QString,
        sort: QString,
        group: QString,
        filter_json: QString,
        columns: i32,
    ) {
        crate::local_albums_model_qt::reset(
            search.to_string(),
            sort.to_string(),
            group.to_string(),
            filter_json.to_string(),
            columns,
        );
    }

    pub fn albums_native_page_miss(self: Pin<&mut Self>, page: i32, generation: i32) {
        crate::local_albums_model_qt::request_page(page, generation);
    }

    pub fn albums_native_toggle_select(self: Pin<&mut Self>, index: i64, shift: bool) {
        crate::local_albums_model_qt::toggle_selection(index, shift);
    }

    pub fn albums_native_select_all(self: Pin<&mut Self>) {
        crate::local_albums_model_qt::select_all();
    }

    pub fn albums_native_clear_selection(self: Pin<&mut Self>) {
        crate::local_albums_model_qt::clear_selection();
    }

    pub fn albums_native_bulk_action(self: Pin<&mut Self>, action: QString) {
        crate::local_albums_model_qt::bulk_action(action.to_string());
    }

    pub fn artists_native_reset(
        self: Pin<&mut Self>,
        search: QString,
        sort: QString,
        filter_json: QString,
    ) {
        if !crate::local_artists_model_qt::reset(
            search.to_string(),
            sort.to_string(),
            filter_json.to_string(),
        ) {
            crate::local_bridge_ops::load_artists_legacy();
            crate::local_bridge_ops::load_albums_legacy();
        }
    }

    pub fn artists_native_page_miss(self: Pin<&mut Self>, page: i32, generation: i32) {
        crate::local_artists_model_qt::request_page(page, generation);
    }

    pub fn artists_native_select(self: Pin<&mut Self>, artist: QString, columns: i32) {
        crate::local_artists_model_qt::select_artist(artist.to_string(), columns);
    }

    pub fn artist_albums_native_page_miss(self: Pin<&mut Self>, page: i32, generation: i32) {
        crate::local_artists_model_qt::request_detail_page(page, generation);
    }

    pub fn tracks_search(self: Pin<&mut Self>, query: QString) {
        lib::set_tracks_query(&query.to_string());
        load_tracks(true);
    }

    pub fn tracks_set_sort(self: Pin<&mut Self>, sort: QString) {
        let sort = sort.to_string();
        lib::set_tracks_sort(&sort);
        ui(move |mut b| {
            b.as_mut()
                .set_local_tracks_sort(QString::from(sort.as_str()));
        });
        load_tracks(true);
    }

    pub fn tracks_set_filter_json(self: Pin<&mut Self>, json: QString) {
        let json = json.to_string();
        lib::set_tracks_filter(&json);
        ui(move |mut b| {
            b.as_mut()
                .set_local_tracks_filter(QString::from(json.as_str()));
        });
        load_tracks(true);
    }

    pub fn tracks_set_group(self: Pin<&mut Self>, mode: QString) {
        let mode = mode.to_string();
        lib::set_tracks_group(&mode);
        ui(move |mut b| {
            b.as_mut()
                .set_local_tracks_group(QString::from(mode.as_str()));
        });
        // Grouping changes the global page order in both readers. Reset the
        // query so page one is already the immutable prefix of the final
        // grouped result; sorting accumulated pages in QML makes the viewport
        // jump whenever new rows belong ahead of it.
        load_tracks(true);
    }

    pub fn tracks_load_more(self: Pin<&mut Self>) {
        if !lib::tracks_has_more() {
            return;
        }
        load_tracks(false);
    }

    pub fn tracks_native_page_miss(self: Pin<&mut Self>, page: i32, generation: i32) {
        crate::local_tracks_model_qt::request_page(page, generation);
    }

    pub fn tracks_native_toggle_select(self: Pin<&mut Self>, index: i64, shift: bool) {
        crate::local_tracks_model_qt::toggle_selection(index, shift);
    }

    pub fn tracks_native_select_all(self: Pin<&mut Self>) {
        crate::local_tracks_model_qt::select_all();
    }

    pub fn tracks_native_clear_selection(self: Pin<&mut Self>) {
        crate::local_tracks_model_qt::clear_selection();
    }

    pub fn tracks_native_play(self: Pin<&mut Self>, index: i64) {
        crate::local_tracks_model_qt::play(index);
    }

    pub fn tracks_native_enqueue(self: Pin<&mut Self>, index: i64, mode: QString) {
        crate::local_tracks_model_qt::enqueue(index, mode.to_string());
    }

    pub fn tracks_native_row_action(self: Pin<&mut Self>, index: i64, action: QString) {
        crate::local_tracks_model_qt::row_action(index, action.to_string());
    }

    pub fn tracks_native_bulk_action(self: Pin<&mut Self>, action: QString) {
        crate::local_tracks_model_qt::bulk_action(action.to_string());
    }

    pub fn tree_toggle(self: Pin<&mut Self>, path: QString, expand: bool) {
        let path = path.to_string();
        if !expand {
            lib::tree_collapse(&path);
            publish_tree(lib::to_json(&lib::tree_visible()));
            return;
        }
        crate::spawn(async move {
            let _ = tokio::task::spawn_blocking(move || lib::tree_expand_blocking(&path)).await;
            publish_tree(lib::to_json(&lib::tree_visible()));
        });
    }

    pub fn tree_collapse_all(self: Pin<&mut Self>) {
        lib::tree_collapse_all();
        publish_tree(lib::to_json(&lib::tree_visible()));
    }

    pub fn tree_search(self: Pin<&mut Self>, query: QString) {
        lib::set_tree_search(&query.to_string());
        publish_tree(lib::to_json(&lib::tree_visible()));
    }

    pub fn select_folder(self: Pin<&mut Self>, path: QString) {
        let path = path.to_string();
        ui(|mut b| b.as_mut().set_local_detail_loading(true));
        crate::spawn(async move {
            let detail =
                tokio::task::spawn_blocking(move || lib::load_folder_detail_blocking(&path))
                    .await
                    .ok();
            let json = detail.map(|d| lib::to_json(&d)).unwrap_or_default();
            ui(move |mut b| {
                b.as_mut()
                    .set_local_detail_json(QString::from(json.as_str()));
                b.as_mut().set_local_detail_loading(false);
            });
        });
    }

    pub fn open_album(self: Pin<&mut Self>, id: QString) {
        open_album_by_id(id.to_string());
    }

    pub fn open_album_filtered(self: Pin<&mut Self>, id: QString, filter_json: QString) {
        open_album_by_id_filtered(id.to_string(), filter_json.to_string());
    }

    pub fn close_album(self: Pin<&mut Self>) {
        ui(|mut b| {
            b.as_mut().set_local_album_json(QString::from(""));
            b.as_mut().set_local_album_loading(false);
        });
    }

    pub fn play_album(self: Pin<&mut Self>, id: QString, shuffle: bool) {
        let id = id.to_string();
        let runtime = crate::app();
        crate::spawn(async move {
            lib::play_album(&runtime, id, None, shuffle).await;
        });
    }

    pub fn play_album_filtered(
        self: Pin<&mut Self>,
        id: QString,
        filter_json: QString,
        shuffle: bool,
    ) {
        let (id, filter_json) = (id.to_string(), filter_json.to_string());
        let runtime = crate::app();
        crate::spawn(async move {
            lib::play_album_filtered(&runtime, id, filter_json, shuffle).await;
        });
    }

    pub fn play_album_track(self: Pin<&mut Self>, id: QString, track_id: QString) {
        let id = id.to_string();
        let row = track_id.to_string().parse::<i64>().ok();
        let runtime = crate::app();
        crate::spawn(async move {
            lib::play_album(&runtime, id, row, false).await;
        });
    }

    pub fn play_folder(self: Pin<&mut Self>, path: QString) {
        let path = path.to_string();
        let runtime = crate::app();
        crate::spawn(async move {
            lib::play_folder(&runtime, path).await;
        });
    }

    pub fn play_folder_track(self: Pin<&mut Self>, path: QString, track_id: QString) {
        let path = path.to_string();
        let Ok(row) = track_id.to_string().parse::<i64>() else {
            return;
        };
        let runtime = crate::app();
        crate::spawn(async move {
            lib::play_folder_track(&runtime, path, row).await;
        });
    }

    pub fn play_tracks_visible(self: Pin<&mut Self>, visible_ids_json: QString, track_id: QString) {
        let Ok(row) = track_id.to_string().parse::<i64>() else {
            return;
        };
        let ids = visible_ids_json.to_string();
        let runtime = crate::app();
        crate::spawn(async move {
            lib::play_tracks_visible(&runtime, ids, row).await;
        });
    }

    pub fn enqueue(self: Pin<&mut Self>, kind: QString, id: QString, mode: QString) {
        let (kind, id, mode) = (kind.to_string(), id.to_string(), mode.to_string());
        let runtime = crate::app();
        crate::spawn(async move {
            lib::enqueue(&runtime, kind, id, mode).await;
        });
    }

    pub fn enqueue_album_filtered(
        self: Pin<&mut Self>,
        id: QString,
        filter_json: QString,
        mode: QString,
    ) {
        let (id, filter_json, mode) = (id.to_string(), filter_json.to_string(), mode.to_string());
        let runtime = crate::app();
        crate::spawn(async move {
            lib::enqueue_album_filtered(&runtime, id, filter_json, mode).await;
        });
    }

    pub fn album_toggle_favorite(
        self: Pin<&mut Self>,
        id: QString,
        title: QString,
        artist: QString,
        artwork_url: QString,
        sources_json: QString,
    ) {
        crate::local_album_actions::toggle_album_favorite(
            id.to_string(),
            title.to_string(),
            artist.to_string(),
            artwork_url.to_string(),
            sources_json.to_string(),
        );
    }

    // --- Plex --------------------------------------------------------------

    pub fn sync_plex(self: Pin<&mut Self>) {
        if plex::is_syncing() {
            return;
        }
        run_sync();
    }

    // ── Media servers (Jellyfin / Subsonic) ─────────────────────────────

    pub fn set_albums_filter_json(mut self: Pin<&mut Self>, json: QString) {
        let text = json.to_string();
        // The qproperty is what the view re-seeds from on its next mount, so
        // it is written on EVERY toggle, not only on close.
        self.as_mut()
            .set_albums_filter(QString::from(text.as_str()));
        crate::local_bridge_ops::save_albums_filter(&text);
    }

    pub fn media_test(self: Pin<&mut Self>, server: QString, url: QString) {
        let Some(kind) = media_kind(&server) else {
            return;
        };
        let url = url.to_string();
        if url.trim().is_empty() {
            return;
        }
        let _ = crate::media_servers_qt::get(kind);
        let Some(operation) = crate::media_connection_qt::Operation::begin(kind, "testing") else {
            return;
        };
        crate::spawn(async move {
            let result = operation
                .request(
                    crate::media_servers_qt::probe(kind, url.trim()),
                    std::time::Duration::from_secs(15),
                )
                .await;
            match result {
                Some(Ok(_)) => operation.phase("reachable", ""),
                Some(Err(error)) => {
                    log::warn!(
                        "[media-connection] provider={} phase=testing failed: {error}",
                        kind.as_str()
                    );
                    operation.phase("failed", &error);
                    crate::toast_qt::error(error);
                }
                None => {}
            }
        });
    }

    pub fn media_cancel_quick_connect(self: Pin<&mut Self>) {
        crate::media_connection_qt::cancel_pairing();
    }
    pub fn media_quick_connect(self: Pin<&mut Self>, url: QString) {
        let kind = qbz_app::settings::media_servers::MediaServerKind::Jellyfin;
        let url = url.to_string();
        if url.trim().is_empty() {
            return;
        }
        let cfg = crate::media_servers_qt::get(kind);
        let Some(operation) = crate::media_connection_qt::Operation::begin(kind, "pairing-start")
        else {
            return;
        };
        crate::spawn(async move {
            let result = operation
                .request(
                    crate::media_servers_qt::quick_connect(url.trim(), cfg, &operation),
                    std::time::Duration::from_secs(350),
                )
                .await;
            match result {
                Some(Ok(cfg)) => {
                    if operation
                        .commit(|| crate::media_servers_qt::put(kind, &cfg))
                        .is_none()
                    {
                        return;
                    }
                    operation.phase("connected", "");
                    crate::settings_qt::publish_snapshot().await;
                    if operation.current() {
                        run_media_sync(kind, true, &operation).await;
                    }
                }
                Some(Err(error)) => {
                    log::warn!(
                        "[media-connection] provider=jellyfin phase=quick-connect failed: {error}"
                    );
                    operation.phase("failed", &error);
                    crate::toast_qt::error(error);
                }
                None => {}
            }
        });
    }

    pub fn media_connect(
        self: Pin<&mut Self>,
        server: QString,
        url: QString,
        username: QString,
        password: QString,
    ) {
        let Some(kind) = media_kind(&server) else {
            return;
        };
        let (url, user, pass) = (url.to_string(), username.to_string(), password.to_string());
        if url.trim().is_empty() || user.trim().is_empty() || pass.is_empty() {
            return;
        }
        let cfg = crate::media_servers_qt::get(kind);
        let Some(operation) = crate::media_connection_qt::Operation::begin(kind, "authenticating")
        else {
            return;
        };
        crate::spawn(async move {
            let result = operation
                .request(
                    crate::media_servers_qt::connect(
                        kind,
                        url.trim(),
                        user.trim(),
                        &pass,
                        cfg,
                        &operation,
                    ),
                    std::time::Duration::from_secs(35),
                )
                .await;
            match result {
                Some(Ok(cfg)) => {
                    if operation
                        .commit(|| crate::media_servers_qt::put(kind, &cfg))
                        .is_none()
                    {
                        return;
                    }
                    operation.phase("connected", "");
                    crate::settings_qt::publish_snapshot().await;
                    if operation.current() {
                        run_media_sync(kind, true, &operation).await;
                    }
                }
                Some(Err(error)) => {
                    log::warn!(
                        "[media-connection] provider={} phase=connect failed: {error}",
                        kind.as_str()
                    );
                    operation.phase("failed", &error);
                    crate::toast_qt::error(error);
                }
                None => {}
            }
        });
    }

    pub fn media_set_enabled(self: Pin<&mut Self>, server: QString, enabled: bool) {
        let Some(kind) = media_kind(&server) else {
            return;
        };
        // Invalidate pending authentication before changing the stored switch.
        // A late response must not silently re-enable a disabled integration.
        crate::media_connection_qt::cancel(kind);
        if !enabled {
            crate::media_sync_qt::cancel(kind);
        }
        let mut cfg = crate::media_servers_qt::get(kind);
        cfg.enabled = enabled;
        crate::media_servers_qt::put(kind, &cfg);
        crate::spawn(async move {
            crate::settings_qt::publish_snapshot().await;
            reload_browse();
        });
    }

    pub fn media_sync(self: Pin<&mut Self>, server: QString, full: bool) {
        let Some(kind) = media_kind(&server) else {
            return;
        };
        let Some(operation) = crate::media_connection_qt::Operation::begin(kind, "syncing") else {
            return;
        };
        crate::spawn(async move {
            run_media_sync(kind, full, &operation).await;
        });
    }

    pub fn media_disconnect(self: Pin<&mut Self>, server: QString) {
        let Some(kind) = media_kind(&server) else {
            return;
        };
        crate::media_connection_qt::cancel(kind);
        // Invalidate credentials before another event can start a connection.
        crate::media_servers_qt::disconnect(kind);
        let user = qbz_app::user_data::UserDataPaths::load_last_user_id();
        crate::spawn(async move {
            crate::settings_qt::publish_snapshot().await;
            // A large cache purge must not block the UI. The state gate also
            // protects against a profile rebind while this job owns the cache.
            let _ = tokio::task::spawn_blocking(move || {
                let _guard = crate::media_sync_qt::media_server_state_guard(kind);
                if qbz_app::user_data::UserDataPaths::load_last_user_id() == user
                    && !crate::media_servers_qt::get(kind).is_configured(kind)
                {
                    crate::media_servers_qt::purge_cache(kind);
                }
            })
            .await;
            if qbz_app::user_data::UserDataPaths::load_last_user_id() == user {
                reload_browse();
            }
        });
    }

    pub fn plex_set_enabled(self: Pin<&mut Self>, enabled: bool) {
        crate::spawn(async move {
            let _ = tokio::task::spawn_blocking(move || plex::set_enabled(enabled)).await;
            publish_plex_state();
            // The union IS the query — the grid/tracks/badges must re-run.
            reload_browse();
        });
    }

    pub fn plex_url_is_local(self: Pin<&mut Self>, url: QString) -> bool {
        plex::is_local_address(&url.to_string())
    }

    pub fn plex_generate_code(self: Pin<&mut Self>, server_url: QString) {
        let url = server_url.to_string();
        crate::spawn(async move { crate::plex_pin_qt::generate_code(url).await });
    }

    pub fn plex_open_auth_url(self: Pin<&mut Self>) {
        crate::spawn(async move { crate::plex_pin_qt::open_auth_url().await });
    }

    pub fn plex_copy_code(self: Pin<&mut Self>) {
        let code = crate::plex_pin_qt::current_code();
        if code.is_empty() {
            return;
        }
        crate::share_qt::copy_to_clipboard(code);
        crate::toast_qt::success(qbz_i18n::t("Code copied"));
    }

    pub fn plex_stop_pin(self: Pin<&mut Self>) {
        crate::plex_pin_qt::stop_poll();
    }

    pub fn plex_check_connection(self: Pin<&mut Self>) {
        crate::spawn(async move { crate::plex_pin_qt::check_connection().await });
    }

    pub fn plex_connect(self: Pin<&mut Self>, server_url: QString, token: QString) {
        let (url, token) = (server_url.to_string(), token.to_string());
        crate::spawn(async move {
            // The LAN gate, enforced BEFORE anything is persisted. Until
            // 2026-08-04 the only thing that produced the error below was an
            // UNPARSEABLE url: `connect_manual` resolves and stores whatever
            // parses, and never consulted `is_local_address` (which had
            // exactly one caller, a read-side gate). So a WAN address was
            // saved silently and the user was told "Plex is not configured"
            // by a later gate — the wrong error for the actual mistake.
            if !plex::is_local_address(&url) {
                // Reusing the Slint warning's msgid rather than the port's own
                // "Plex server must be a local network address", which has no
                // entry in ANY of the eight catalogs and therefore rendered
                // English for everyone. This one ships translated in all
                // seven non-English locales (`en` falls back to the msgid).
                let msg = qbz_i18n::t("Only local network servers are supported.");
                ui(move |mut b| {
                    b.as_mut().set_plex_error(QString::from(msg.as_str()));
                });
                return;
            }
            let base = tokio::task::spawn_blocking(move || {
                plex::set_enabled(true);
                plex::connect_manual(&url, &token)
            })
            .await
            .unwrap_or_default();
            if base.is_empty() {
                // Unusable input that still is not a LAN violation (an
                // unparseable url, or a scheme that is not http/https).
                // Translated Rust-side (qbz_i18n is the same catalog the
                // Slint build ships); QML shows `plexError` verbatim.
                let msg = qbz_i18n::t("Enter a valid server address.");
                ui(move |mut b| {
                    b.as_mut().set_plex_error(QString::from(msg.as_str()));
                });
                return;
            }
            publish_plex_state();
            run_sync();
        });
    }

    pub fn plex_disconnect(self: Pin<&mut Self>) {
        crate::spawn(async move {
            let _ = tokio::task::spawn_blocking(plex::disconnect).await;
            ui(|mut b| {
                b.as_mut().set_plex_last_sync_tracks(-1);
                b.as_mut().set_plex_error(QString::from(""));
            });
            publish_plex_state();
            reload_browse();
        });
    }

    pub fn set_plex_sections(self: Pin<&mut Self>, keys_json: QString) {
        let keys: Vec<String> = serde_json::from_str(&keys_json.to_string()).unwrap_or_default();
        crate::spawn(async move {
            let _ = tokio::task::spawn_blocking(move || plex::set_selected_sections(&keys)).await;
            publish_plex_state();
            run_sync();
        });
    }

    pub fn refresh_plex(self: Pin<&mut Self>) {
        publish_plex_state();
    }

    // --- Artwork -----------------------------------------------------------

    pub fn artwork_timing_enabled(&self) -> bool {
        log::log_enabled!(target: "qbz.nav.timing", log::Level::Debug)
    }

    pub fn artwork_immediate_enabled(&self) -> bool {
        // Diagnostic A/B switch: the same binary can exercise the previous
        // reveal path without changing its compiler, backend or dependencies.
        std::env::var("QBZ_GRID_IMMEDIATE_ART").as_deref() != Ok("0")
    }

    pub fn artwork_timing(&self, stage: QString, key: QString, window: QString) {
        if !self.artwork_timing_enabled() {
            return;
        }
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let elapsed = START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f64()
            * 1000.0;
        log::debug!(target: "qbz.nav.timing",
            "art ms={elapsed:.3} stage={} window={} key={}",
            stage.to_string(), window.to_string(), key.to_string());
    }

    pub fn artwork_window(self: Pin<&mut Self>, keys_json: QString) {
        let keys: Vec<String> = serde_json::from_str(&keys_json.to_string()).unwrap_or_default();
        if keys.is_empty() {
            return;
        }
        // Artist portrait enrichment is driven by this same mounted window:
        // never issue provider requests for every row in the catalog.
        crate::local_artist_images_qt::request_visible(&keys);
        crate::spawn(async move {
            // Phase 1 (cheap: memos + one stat per key, no decode) — emit
            // everything already on disk right away.
            let window = tokio::task::spawn_blocking(move || lib::resolve_window_blocking(keys))
                .await
                .ok();
            let Some(window) = window else {
                return;
            };
            emit_artwork(window.hits);
            // Phase 2, both arms in parallel and both STREAMING: each cover
            // reaches QML through `localArtworkReady` the moment it resolves,
            // so a first visit fills in progressively instead of landing as
            // one lump after the whole window is done.
            //   - cold local covers: bounded blocking pool, started in
            //     display order (row 1 before row 40);
            //   - Plex/http misses: network, independent of the CPU work, so
            //     a slow decode never holds up a downloaded cover.
            // (`stream_cold` is reached directly — `local_library_qt` only
            // re-exports the two entry points the older batch flow used.)
            let cold = crate::local_artwork::stream_cold(window.cold, emit_artwork_one);
            let remote = async {
                let fetched = lib::fetch_plex_misses(window.plex_misses).await;
                emit_artwork(fetched);
            };
            tokio::join!(cold, remote);
        });
    }

    pub fn album_header_atmosphere(self: Pin<&mut Self>, key: QString, path: QString) {
        let key = key.to_string();
        let path = path.to_string();
        if key.is_empty() || path.is_empty() {
            return;
        }
        let raw_path = match crate::artwork_qt::classify(&path) {
            crate::artwork_qt::ArtUrl::LocalFile(path) => path,
            _ => return,
        };
        crate::spawn(async move {
            let tint_path = raw_path.clone();
            let atmo_path = raw_path;
            let (tint, atmosphere) = tokio::join!(
                tokio::task::spawn_blocking(move || {
                    crate::album_qt::header_tint_hex(&tint_path)
                }),
                tokio::task::spawn_blocking(move || {
                    crate::atmosphere_qt::for_cover_blocking(&atmo_path)
                }),
            );
            let tint = tint.ok().flatten().unwrap_or_default();
            let atmosphere = atmosphere.ok().flatten().unwrap_or_default();
            if tint.is_empty() && atmosphere.is_empty() {
                return;
            }
            ui(move |mut b| {
                b.as_mut().local_album_atmosphere_ready(
                    QString::from(key.as_str()),
                    QString::from(path.as_str()),
                    QString::from(tint.as_str()),
                    QString::from(atmosphere.as_str()),
                );
            });
        });
    }
    // --- Tree selection + bulk actions --------------------------------------

    pub fn tree_set_select_mode(self: Pin<&mut Self>, on: bool) {
        crate::local_bulk::set_select_mode(on);
    }

    pub fn tree_toggle_folder_select(self: Pin<&mut Self>, path: QString) {
        crate::local_bulk::toggle_folder_select(path.to_string());
    }

    pub fn tree_toggle_track_select(self: Pin<&mut Self>, path: QString) {
        crate::local_bulk::toggle_track_select(path.to_string());
    }

    pub fn tree_select_range(self: Pin<&mut Self>, nodes_json: QString) {
        crate::local_bulk::select_tree_range(nodes_json.to_string());
    }

    pub fn folders_bulk_action(self: Pin<&mut Self>, action: QString) {
        crate::local_bulk::folders_bulk_action(action.to_string());
    }

    pub fn bulk_action(self: Pin<&mut Self>, scope: QString, ids_json: QString, action: QString) {
        crate::local_bulk::bulk_action(scope.to_string(), ids_json.to_string(), action.to_string());
    }

    pub fn close_media_info(self: Pin<&mut Self>) {
        crate::local_media_info_qt::close();
    }

    pub fn copy_media_info(self: Pin<&mut Self>, value: QString) {
        crate::local_media_info_qt::copy(value.to_string());
    }

    pub fn open_media_info_for_track(self: Pin<&mut Self>, id: i64) {
        crate::local_media_info_qt::open_track_by_row_id(id);
    }

    // --- Local album actions -------------------------------------------------

    pub fn clear_pending_artist(self: Pin<&mut Self>) {
        crate::local_album_actions::clear_pending_artist();
    }

    pub fn set_pending_route(mut self: Pin<&mut Self>, route_json: QString) {
        // Cleared first: the property CHANGE is the trigger, so setting the
        // same route twice in a row would otherwise fire nothing the second
        // time (the same reason `local_ephemeral_open_seq` is a sequence).
        self.as_mut().set_local_pending_route(QString::from(""));
        self.as_mut().set_local_pending_route(route_json);
    }

    pub fn clear_pending_route(self: Pin<&mut Self>) {
        crate::local_album_actions::clear_pending_route();
    }

    pub fn open_artist_by_name(self: Pin<&mut Self>, name: QString) {
        crate::local_album_actions::open_artist_by_name(name.to_string());
    }

    pub fn artist_album_ids(&self, artist: QString) -> QString {
        QString::from(crate::local_albums::artist_album_ids(&artist.to_string()).as_str())
    }

    pub fn genre_album_tracks(_self: Pin<&mut Self>, id: QString, filter_json: QString) {
        let started = std::time::Instant::now();
        let id = id.to_string();
        let filter_json = filter_json.to_string();
        enum Request {
            Cached(String),
            Pending,
            Spawn,
        }
        let request = crate::local_state::state(|state| {
            if state.genre_detail_filters.get(&id) == Some(&filter_json) {
                if let Some(json) = state.genre_detail_docs.get(&id).cloned() {
                    return Request::Cached(json);
                }
            }
            if state.genre_detail_requests.get(&id) == Some(&filter_json) {
                return Request::Pending;
            }
            state
                .genre_detail_requests
                .insert(id.clone(), filter_json.clone());
            // A version switch serialized against the old funnel must not
            // publish over the replacement document that starts here.
            let generation = state
                .genre_detail_version_generations
                .get(&id)
                .copied()
                .unwrap_or(0)
                .wrapping_add(1);
            state
                .genre_detail_version_generations
                .insert(id.clone(), generation);
            Request::Spawn
        });
        match request {
            Request::Cached(json) => {
                queue_genre_album_ready(id, json, filter_json, "cache-hit", started);
                return;
            }
            // The original worker owns the one eventual signal. Spawning a
            // duplicate here used to double the database/map/serialize work
            // for every ListView remount during a filter transition.
            Request::Pending => return,
            Request::Spawn => {}
        }
        crate::spawn(async move {
            let request_id = id.clone();
            let request_filter = filter_json.clone();
            let result = tokio::task::spawn_blocking(move || {
                let fetch_started = std::time::Instant::now();
                let cached_tracks = crate::local_state::state(|state| {
                    state.genre_detail_all_tracks.get(&request_id).cloned()
                });
                let raw_cache_hit = cached_tracks.is_some();
                let all_tracks = cached_tracks.unwrap_or_else(|| {
                    let mut tracks =
                        crate::local_albums::fetch_album_tracks_blocking(&request_id);
                    crate::local_playback::fill_missing_covers(&mut tracks);
                    std::sync::Arc::new(tracks)
                });
                let fetch_elapsed = fetch_started.elapsed();
                let project_started = std::time::Instant::now();
                let filter = crate::local_filter::MediaFilter::from_json(&request_filter);
                // The raw cache is shared: changing a source/quality chip must
                // not deep-clone every physical copy before throwing most of
                // them away. Clone only the rows admitted by this funnel.
                let tracks = all_tracks
                    .iter()
                    .filter(|track| filter.track_enabled(track))
                    .cloned()
                    .collect::<Vec<_>>();
                let filtered_rows = tracks.len();
                let versions = std::sync::Arc::new(
                    crate::local_album_actions::split_versions(tracks),
                );
                let selected = versions
                    .first()
                    .map(|(_, rows)| rows.clone())
                    .unwrap_or_default();
                let doc = crate::local_album_actions::genre_detail_doc(
                    &request_id,
                    versions.as_slice(),
                    0,
                );
                let json = crate::local_rows::to_json(&doc);
                let project_elapsed = project_started.elapsed();
                let versions_empty = versions.is_empty();
                log::info!(
                    "[qbz-qt][perf] genre album detail phase=worker id={} raw_cache={} raw_rows={} filtered_rows={} versions={} fetch={:?} project={:?}",
                    request_id,
                    raw_cache_hit,
                    all_tracks.len(),
                    filtered_rows,
                    doc.versions.len(),
                    fetch_elapsed,
                    project_elapsed,
                );
                let cached_json = json.clone();
                let current = crate::local_state::state(move |state| {
                    if state.genre_detail_requests.get(&request_id) != Some(&request_filter) {
                        return false;
                    }
                    if !all_tracks.is_empty() {
                        if state.genre_detail_all_tracks.len() >= 32
                            && !state.genre_detail_all_tracks.contains_key(&request_id)
                        {
                            if let Some(oldest) =
                                state.genre_detail_all_tracks.keys().min().cloned()
                            {
                                state.genre_detail_raw.remove(&oldest);
                                state.genre_detail_all_tracks.remove(&oldest);
                                state.genre_detail_versions.remove(&oldest);
                                state.genre_detail_docs.remove(&oldest);
                                state.genre_detail_filters.remove(&oldest);
                                state.genre_detail_requests.remove(&oldest);
                                state.genre_detail_version_generations.remove(&oldest);
                            }
                        }
                        state
                            .genre_detail_all_tracks
                            .insert(request_id.clone(), all_tracks);
                    }
                    state.genre_detail_requests.remove(&request_id);
                    // An unavailable NAS/server is not an authoritative empty
                    // album. Let this view render the miss, but keep the old
                    // completed-cache tag (if any) so a later retry is not
                    // mistaken for a completed hit of this funnel.
                    if versions_empty {
                        return true;
                    }
                    state.genre_detail_raw.insert(request_id.clone(), selected);
                    state
                        .genre_detail_versions
                        .insert(request_id.clone(), versions);
                    state
                        .genre_detail_docs
                        .insert(request_id.clone(), cached_json);
                    state
                        .genre_detail_filters
                        .insert(request_id, request_filter);
                    true
                });
                if !current {
                    return None;
                }
                // An unavailable NAS/server is not an authoritative empty
                // album. Publish the miss for this view, but do not make it a
                // process-lifetime cache hit that prevents a later retry.
                if versions_empty {
                    return Some((json, raw_cache_hit));
                }
                Some((json, raw_cache_hit))
            })
            .await
            .ok()
            .flatten();
            if let Some((result, raw_cache_hit)) = result {
                queue_genre_album_ready(
                    id,
                    result,
                    filter_json,
                    if raw_cache_hit {
                        "filter-cache"
                    } else {
                        "query"
                    },
                    started,
                );
            }
        });
    }

    pub fn genre_album_select_version(self: Pin<&mut Self>, album_id: QString, index: i32) {
        let started = std::time::Instant::now();
        let album_id = album_id.to_string();
        let generation = crate::local_state::state(|state| {
            let generation = state
                .genre_detail_version_generations
                .get(&album_id)
                .copied()
                .unwrap_or(0)
                .wrapping_add(1);
            state
                .genre_detail_version_generations
                .insert(album_id.clone(), generation);
            generation
        });
        crate::spawn(async move {
            let request_id = album_id.clone();
            let selection = tokio::task::spawn_blocking(move || {
                let filter_json = crate::local_state::state(|state| {
                    state
                        .genre_detail_filters
                        .get(&request_id)
                        .cloned()
                        .unwrap_or_default()
                });
                let (doc, selected) =
                    crate::local_album_actions::genre_version_selection(&request_id, index)?;
                Some((
                    request_id,
                    selected,
                    crate::local_rows::to_json(&doc),
                    filter_json,
                ))
            })
            .await
            .ok()
            .flatten();
            if let Some((request_id, selected, json, filter_json)) = selection {
                let bytes = json.len();
                let current = crate::local_state::state(|state| {
                    if state
                        .genre_detail_version_generations
                        .get(&request_id)
                        .copied()
                        != Some(generation)
                    {
                        return false;
                    }
                    state.genre_detail_raw.insert(request_id.clone(), selected);
                    state
                        .genre_detail_docs
                        .insert(request_id.clone(), json.clone());
                    true
                });
                if !current {
                    return;
                }
                log::info!(
                    "[qbz-qt][perf] genre album detail phase=version id={} bytes={} elapsed={:?}",
                    request_id,
                    bytes,
                    started.elapsed(),
                );
                ui(move |mut bridge| {
                    bridge.as_mut().local_genre_album_ready(
                        QString::from(request_id.as_str()),
                        QString::from(json.as_str()),
                        QString::from(filter_json.as_str()),
                    );
                });
            }
        });
    }

    pub fn genre_album_action(
        self: Pin<&mut Self>,
        album_id: QString,
        action: QString,
        track_id: QString,
    ) {
        crate::local_album_actions::genre_album_action(
            album_id.to_string(),
            action.to_string(),
            track_id.to_string().parse::<i64>().ok(),
        );
    }

    pub fn album_selected_action(self: Pin<&mut Self>, action: QString, track_id: QString) {
        crate::local_album_actions::selected_album_action(
            action.to_string(),
            track_id.to_string().parse::<i64>().ok(),
        );
    }

    pub fn album_edit_tags(self: Pin<&mut Self>, id: QString) {
        crate::local_album_actions::edit_tags(id.to_string());
    }

    pub fn album_add_to_playlist(self: Pin<&mut Self>, id: QString) {
        crate::local_album_actions::add_to_playlist(id.to_string());
    }

    pub fn album_add_to_mixtape(self: Pin<&mut Self>, id: QString) {
        crate::local_album_actions::add_to_mixtape(id.to_string());
    }

    pub fn album_select_version(self: Pin<&mut Self>, index: i32) {
        crate::local_album_actions::select_version(index);
    }

    pub fn album_disc_action(self: Pin<&mut Self>, disc: i32, action: QString) {
        crate::local_album_actions::disc_action(disc, action.to_string());
    }

    pub fn genre_album_disc_action(
        self: Pin<&mut Self>,
        album_id: QString,
        disc: i32,
        action: QString,
    ) {
        crate::local_album_actions::genre_disc_action(
            album_id.to_string(),
            disc,
            action.to_string(),
        );
    }

    // --- Ephemeral folder ----------------------------------------------------

    pub fn ephemeral_open(self: Pin<&mut Self>) {
        crate::local_ephemeral::open();
    }

    pub fn ephemeral_open_path(self: Pin<&mut Self>, path: QString) {
        crate::local_ephemeral::open_path(path.to_string());
    }

    pub fn ephemeral_clear(self: Pin<&mut Self>) {
        crate::local_ephemeral::clear();
    }

    pub fn ephemeral_open_cd(self: Pin<&mut Self>) {
        // Reading a TOC spins the drive up and can take a second or two, so it
        // does not run on the UI thread.
        crate::spawn(async move {
            let _busy = crate::local_ephemeral::OpenBusy::begin();
            match crate::cdda_qt::open_disc().await {
                Ok(n) => log::info!("[qbz-qt] cd opened: {n} tracks"),
                Err(msg) => crate::toast_qt::error(msg),
            }
        });
    }

    pub fn ephemeral_open_sacd(self: Pin<&mut Self>) {
        crate::spawn(async move {
            let _busy = crate::local_ephemeral::OpenBusy::begin();
            // NOT `spawn_blocking`: `rfd`'s async dialog posts itself to the
            // thread the platform requires (the main one, on macOS), which the
            // blocking API does not.
            let Some(path) = crate::local_ephemeral::pick_image_blocking().await else {
                return;
            };
            let p = std::path::PathBuf::from(&path);
            // Reading the area TOC seeks around a multi-gigabyte file.
            let outcome = tokio::task::spawn_blocking(move || crate::sacd_qt::open_image(&p)).await;
            match outcome {
                Ok(Ok(n)) => log::info!("[qbz-qt] sacd opened: {n} tracks"),
                Ok(Err(msg)) => crate::toast_qt::error(msg),
                Err(e) => log::warn!("[qbz-qt] sacd open task failed: {e}"),
            }
        });
    }

    pub fn rip_wizard_open(self: Pin<&mut Self>) {
        crate::rip_wizard_qt::open();
    }

    pub fn rip_wizard_close(self: Pin<&mut Self>) {
        crate::rip_wizard_qt::close();
    }

    pub fn rip_pick_destination(self: Pin<&mut Self>) {
        crate::rip_wizard_qt::pick_destination();
    }

    pub fn rip_start(self: Pin<&mut Self>, edits_json: QString) {
        crate::rip_wizard_qt::start(&edits_json.to_string());
    }

    pub fn rip_panel(self: Pin<&mut Self>, open: bool) {
        crate::rip_qt::set_panel_open(open);
    }

    pub fn rip_cancel(self: Pin<&mut Self>) {
        crate::rip_qt::cancel();
    }

    pub fn ephemeral_play_all(self: Pin<&mut Self>, shuffle: bool) {
        let runtime = crate::app();
        crate::spawn(async move {
            crate::local_ephemeral::play_all(&runtime, shuffle).await;
        });
    }

    pub fn ephemeral_play_album(self: Pin<&mut Self>, group_key: QString) {
        let key = group_key.to_string();
        let runtime = crate::app();
        crate::spawn(async move {
            crate::local_ephemeral::play_album(&runtime, key).await;
        });
    }

    pub fn ephemeral_edit_tags(self: Pin<&mut Self>, group_key: QString) {
        crate::tag_editor_qt::open_ephemeral(group_key.to_string());
    }

    pub fn ephemeral_play_track(self: Pin<&mut Self>, id: QString) {
        let Ok(row) = id.to_string().parse::<i64>() else {
            return;
        };
        let runtime = crate::app();
        crate::spawn(async move {
            crate::local_ephemeral::play_track(&runtime, row).await;
        });
    }
}

// ---------------------------------------------------------------------------
// Local album routing
// ---------------------------------------------------------------------------

/// Open a LOCAL/Plex album. Lifted out of the invokable so `open_album` in
/// main.rs can route to it when a local id reaches the catalog path.
pub(crate) fn open_album_by_id(id: String) {
    open_album_by_id_filtered(id, String::new());
}

pub(crate) fn open_album_by_id_filtered(id: String, filter_json: String) {
    open_album_detail(id, filter_json, false);
}

pub(crate) fn restore_album(route: crate::local_restore_qt::AlbumRoute) {
    open_album_detail(route.id, route.filter_json, true);
}

static ALBUM_LOAD_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn open_album_detail(id: String, filter_json: String, restoring: bool) {
    use std::sync::atomic::Ordering;
    let revision = ALBUM_LOAD_REVISION.fetch_add(1, Ordering::AcqRel) + 1;
    let profile = crate::local_state::db_path();
    crate::local_restore_qt::note_album_request(&id, &filter_json);
    ui(|mut b| {
        // Clear the previous local album with the loading flag — the same
        // stale-render the catalog album view had.
        b.as_mut().set_local_album_json(QString::from(""));
        b.as_mut().set_local_album_loading(true);
    });
    crate::spawn(async move {
        let detail = tokio::task::spawn_blocking(move || {
            lib::load_album_detail_filtered_blocking(&id, &filter_json)
        })
        .await
        .ok()
        .flatten();
        let json = detail.map(|d| lib::to_json(&d)).unwrap_or_default();
        ui(move |mut b| {
            // A slow restore cannot publish over a newer album request or
            // redirect a different profile/page after the user moved on.
            if ALBUM_LOAD_REVISION.load(Ordering::Acquire) != revision
                || crate::local_state::db_path() != profile
            {
                return;
            }
            let missing = json.is_empty();
            b.as_mut()
                .set_local_album_json(QString::from(json.as_str()));
            b.as_mut().set_local_album_loading(false);
            if restoring && missing && crate::nav_qt::current_view() == "localalbum" {
                log::info!("[qbz-qt] remembered local album unavailable; opening Local Library");
                crate::navigate_to("local");
            }
        });
    });
}

/// Map the QML `server` word to a kind, toasting on an unknown one.
///
/// A silent `return` would make a typo in QML look like a dead button; this at
/// least says which word was not understood.
fn media_kind(server: &QString) -> Option<qbz_app::settings::media_servers::MediaServerKind> {
    let w = server.to_string();
    let kind = qbz_app::settings::media_servers::MediaServerKind::from_word(w.trim());
    if kind.is_none() {
        log::error!("[qbz-qt] media server: unknown server word {w:?}");
    }
    kind
}

/// Run a sweep and report it, then reload the browse documents so the new rows
/// appear without the user navigating away and back.
async fn run_media_sync(
    kind: qbz_app::settings::media_servers::MediaServerKind,
    full: bool,
    operation: &crate::media_connection_qt::Operation,
) {
    if !operation.current() {
        return;
    }
    operation.phase("syncing", "");
    use qbz_app::settings::media_servers::MediaServerKind;
    let result = match kind {
        MediaServerKind::Jellyfin => crate::media_sync_qt::sync_jellyfin(full)
            .await
            .map_err(crate::media_sync_qt::SyncError::Failed),
        MediaServerKind::Subsonic => crate::media_sync_qt::sync_subsonic(full).await,
    };
    if !operation.current() {
        return;
    }
    match result {
        Ok(r) => {
            operation.phase("ready", "");
            log::info!(
                "[qbz-qt] {} sync: {} saved, {} pruned, {} cached",
                kind.as_str(),
                r.saved,
                r.pruned,
                r.total
            );
            // The sweep stamped `last_sync_at` / `last_sync_tracks`; without a
            // republish the panel keeps reporting "Not synced yet".
            crate::settings_qt::publish_snapshot().await;
            reload_browse();
        }
        Err(crate::media_sync_qt::SyncError::ServerRefreshing { server, scanned }) => {
            operation.phase("refreshing", "");
            crate::toast_qt::info(qbz_i18n::t_args(
                "{} is refreshing its library ({} items scanned). Wait for it to finish, then sync again.",
                &[&server, &scanned.to_string()],
            ));
        }
        Err(crate::media_sync_qt::SyncError::Failed(error)) => {
            let message = qbz_i18n::t_args("Library sync failed: {}", &[&error]);
            log::warn!(
                "[media-connection] provider={} phase=syncing failed: {error}",
                kind.as_str()
            );
            operation.phase("sync-failed", &message);
            crate::toast_qt::error(message);
        }
    }
}
