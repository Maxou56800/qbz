//! QbzMyQbz — MyQBZ domain bridge: both grids, the collection/mixtape detail
//! view and the three MyQBZ modals (create / edit / mix), plus the nav branding
//! document (phase-23 per-domain pattern; documented in main.rs).
//!
//! The module is `qbz_myqbz_bridge`, NEVER `qbz_mixtape`: bridge modules are
//! never named after a workspace crate — this crate depends on `qbz-mixtape`,
//! whose Rust name is `qbz_mixtape` (the `qbz_library` collision is the
//! documented precedent, library_bridge.rs:1-6). The QML type name comes from
//! the QObject (`QbzMyQbz`), not from the module, so QML is unaffected.
//!
//! Props: seven JSON documents, one per surface (spec 02 §4.1). TWO grid
//! properties on purpose: `nav_qt::back()` publishes only `currentView` and
//! never re-dispatches a load, so a single discriminated document would render
//! the Collections rows under the Mixtapes route after mixtapes → collections →
//! back. Two properties make that impossible and are 1:1 with Slint's two
//! models (`state.slint:2575,2578`).
//!
//! `branding_json` is seeded in `Default`, not at `boot()`: `NavFlyout.qml`
//! binds the section label and the first frame is already too late for a
//! post-boot push — the same reason `theme_json` / `window_width` are seeded at
//! construction (shell_bridge.rs "Seeded at CONSTRUCTION").
//!
//! ONE `#[qsignal]`, `detail_rows_patched`: every user-visible outcome goes
//! through `QbzShell.toastJson` (toast_qt.rs) and every state change is a
//! property republish, so the auto-generated `<prop>Changed` signals are
//! normally the only `Connections` targets QML needs. The exception is the
//! detail list's per-row DELTA channel, where republishing the whole document
//! per row was measured quadratic — the reasoning is at the signal's own
//! declaration, and `QbzHome.foryouArtReady` is the precedent.
//!
//! Invokable bodies are one-line forwards into the domain controllers, exactly
//! as album_bridge.rs forwards. Nothing here holds state or does work.

use std::pin::Pin;
use std::sync::OnceLock;

use cxx_qt::CxxQtThread;
use cxx_qt::Threading as _;
use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod qbz_myqbz_bridge {
    extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        // --- Grids (myqbz_qt.rs GridDoc; `loading`/`search`/`sort`/`sortDir`/
        //     `view`/`kindFilter` all live INSIDE the document) ---------------
        #[qproperty(QString, mixtapes_json)]
        #[qproperty(QString, collections_json)]
        // --- Collection / mixtape detail (myqbz_detail_qt.rs DetailDoc) ------
        #[qproperty(QString, detail_json)]
        // --- Modals: create / edit (rename|description|delete) / DJ mix -----
        #[qproperty(QString, create_json)]
        #[qproperty(QString, edit_json)]
        #[qproperty(QString, mix_json)]
        // --- Nav branding (label + custom icon), seeded at construction -----
        #[qproperty(QString, branding_json)]
        type QbzMyQbz = super::QbzMyQbzRust;

        /// Registers this object's Qt-thread hop (Main.qml boots EVERY domain
        /// singleton; only QbzSession.boot also fires crate::on_boot).
        #[qinvokable]
        fn boot(self: Pin<&mut QbzMyQbz>);

        // --- Grids ----------------------------------------------------------

        /// Grid card click → nav push + load the detail document.
        #[qinvokable]
        fn open_card(self: Pin<&mut QbzMyQbz>, id: QString);
        /// Collapsible-sidebar row menu: hide (or unhide) a collection from the
        /// sidebar tree — the same "hide from sidebar" affordance playlists use.
        /// The grids and the management listing keep showing it.
        #[qinvokable]
        fn set_collection_hidden(self: Pin<&mut QbzMyQbz>, id: QString, hidden: bool);
        /// `grid` is "mixtapes" | "collections" (the document's own identity).
        #[qinvokable]
        fn grid_search(self: Pin<&mut QbzMyQbz>, grid: QString, query: QString);
        /// `field` ∈ position | name | items | updated; re-picking the same
        /// field flips the direction, a new field resets to asc.
        #[qinvokable]
        fn grid_set_sort(self: Pin<&mut QbzMyQbz>, grid: QString, field: QString);
        /// `view` ∈ grid | list (session-scoped, Rust-owned, not persisted).
        #[qinvokable]
        fn grid_set_view(self: Pin<&mut QbzMyQbz>, grid: QString, view: QString);
        /// Collections grid only: all | collection | artist_collection.
        #[qinvokable]
        fn grid_set_kind_filter(self: Pin<&mut QbzMyQbz>, kind: QString);
        /// Clears search + sort (+ the kind filter for collections).
        #[qinvokable]
        fn grid_reset(self: Pin<&mut QbzMyQbz>, grid: QString);

        /// Kiosk-only demand artwork; bounds are inclusive, pixels physical.
        #[qinvokable]
        fn kiosk_grid_artwork(
            self: Pin<&mut QbzMyQbz>,
            grid: QString,
            first: i32,
            last: i32,
            px: i32,
        );
        #[qinvokable]
        fn kiosk_detail_artwork(self: Pin<&mut QbzMyQbz>, first: i32, last: i32, px: i32);

        // --- Create modal ---------------------------------------------------

        /// "+ New Mixtape" / "+ New Collection" (`kind` ∈ mixtape | collection).
        #[qinvokable]
        fn create_open(self: Pin<&mut QbzMyQbz>, kind: QString);
        /// Radio flip inside the modal.
        #[qinvokable]
        fn create_set_kind(self: Pin<&mut QbzMyQbz>, kind: QString);
        /// Rust caps the name at 80 Unicode scalars, creates, then navigates to
        /// the new detail. The draft name lives in QML (`TextField.text`).
        #[qinvokable]
        fn create_submit(self: Pin<&mut QbzMyQbz>, name: QString);
        #[qinvokable]
        fn create_close(self: Pin<&mut QbzMyQbz>);

        // --- Detail: toolbar ------------------------------------------------

        /// Transient — the only detail mutation that does NOT persist prefs.
        #[qinvokable]
        fn detail_search(self: Pin<&mut QbzMyQbz>, query: QString);
        /// `field` ∈ position | name | year | tracks.
        #[qinvokable]
        fn detail_set_sort(self: Pin<&mut QbzMyQbz>, field: QString);
        /// `value` ∈ all | album | track | playlist.
        #[qinvokable]
        fn detail_set_type_filter(self: Pin<&mut QbzMyQbz>, value: QString);
        /// Multi-select source filter: qobuz | plex | local.
        #[qinvokable]
        fn detail_toggle_source_filter(self: Pin<&mut QbzMyQbz>, kind: QString);
        /// `mode` ∈ list | grid | expanded; `expanded` also fires
        /// `ensure_expanded`. No re-derive — caches, selection and scroll
        /// position all survive (myqbz_detail.rs:558-562). Since the accordion
        /// landed, `expanded` = OPEN ALL and leaving it = close all, through
        /// the same per-row state `detail_toggle_row_expand` writes.
        #[qinvokable]
        fn detail_set_view_mode(self: Pin<&mut QbzMyQbz>, mode: QString);
        /// THE CHEVRON: toggle ONE row's inline block open/closed, by its
        /// `sourceItemId`. A first open fetches only that row's tracks; a
        /// second is instant off `INLINE_CACHE`. Non-expandable rows (tracks)
        /// are a no-op. Owner-authorised divergence from both references,
        /// which have no per-row accordion at all.
        #[qinvokable]
        fn detail_toggle_row_expand(self: Pin<&mut QbzMyQbz>, source_item_id: QString);
        /// Re-publish the open detail document unchanged, for a FRESH view
        /// instance. `AppShell.qml:192` mounts this view through a `Loader`, so
        /// nav-away destroys it; the `detail_rows_patched` deltas below were
        /// delivered to the dead instance and the new one would come back with
        /// stale quality badges, no inline tracks and every row collapsed.
        /// Called from `Component.onCompleted` — once per mount.
        #[qinvokable]
        fn detail_resync(self: Pin<&mut QbzMyQbz>);
        /// Leaving select mode clears the selection.
        #[qinvokable]
        fn detail_toggle_select_mode(self: Pin<&mut QbzMyQbz>);
        /// `position` is the 0-based persisted position (the stable key);
        /// `shift` = the click held Shift, i.e. add the range from the last
        /// plain click (myqbz_detail_qt::toggle_item_select).
        #[qinvokable]
        fn detail_toggle_item_select(self: Pin<&mut QbzMyQbz>, position: i32, shift: bool);
        /// Resets type/source/sort; the search text is left INTACT
        /// (myqbz_detail.rs:540-541).
        #[qinvokable]
        fn detail_reset_filters(self: Pin<&mut QbzMyQbz>);

        // --- Detail: navigation out of a row --------------------------------

        /// album/track → open_album; playlist → open_playlist.
        #[qinvokable]
        fn open_item(
            self: Pin<&mut QbzMyQbz>,
            source: QString,
            item_type: QString,
            source_item_id: QString,
        );
        /// qobuz + non-empty id → open_artist(id); qobuz + empty id → log and
        /// ignore (never fall back to the name); local/plex → LocalLibrary
        /// Artists by name.
        #[qinvokable]
        fn open_artist(
            self: Pin<&mut QbzMyQbz>,
            source: QString,
            artist_name: QString,
            artist_id: QString,
        );

        // --- Detail: hero actions -------------------------------------------

        #[qinvokable]
        fn play_all(self: Pin<&mut QbzMyQbz>);
        #[qinvokable]
        fn shuffle(self: Pin<&mut QbzMyQbz>);
        /// Opens the DJ-mix ("Random queue") modal.
        #[qinvokable]
        fn dj_mix(self: Pin<&mut QbzMyQbz>);
        /// `artist_collection` only, and a logged no-op by design:
        /// artist_discography is a MARKER, there is no sync command
        /// (myqbz_builder.rs:9-12, main.rs:6386-6392).
        #[qinvokable]
        fn sync(self: Pin<&mut QbzMyQbz>);

        // --- Detail: per-row actions ----------------------------------------

        #[qinvokable]
        fn play_item(self: Pin<&mut QbzMyQbz>, source_item_id: QString);
        /// `action` ∈ play | play-next | play-later | add-to-queue (the
        /// underscore spellings are accepted too, myqbz_play.rs:59-67).
        #[qinvokable]
        fn item_action(self: Pin<&mut QbzMyQbz>, source_item_id: QString, action: QString);
        /// Routes through the bulk remover with a 1-element vec.
        #[qinvokable]
        fn remove_item(self: Pin<&mut QbzMyQbz>, position: i32);
        /// Bulk bar. Accepts, and the bar renders in this order:
        /// add-to-queue · play-later · play-next · add-to-playlist ·
        /// add-to-mixtape · remove-selected (danger) · clear
        /// (MixtapeDetailView.slint:1258-1266 — note LATER before NEXT, the
        /// opposite of the row menu).
        #[qinvokable]
        fn bulk_action(self: Pin<&mut QbzMyQbz>, id: QString);
        /// Expanded mode: resolve the inline track lists (idempotent).
        #[qinvokable]
        fn ensure_expanded(self: Pin<&mut QbzMyQbz>);
        /// `action` ∈ play | play-next | play-later | go-to-album.
        #[qinvokable]
        fn inline_track_action(
            self: Pin<&mut QbzMyQbz>,
            item_source_item_id: QString,
            track_id: QString,
            action: QString,
        );

        // --- Detail: overflow menu → edit modal / cover ----------------------

        /// Fills `editJson.mode = "rename"`.
        #[qinvokable]
        fn open_rename(self: Pin<&mut QbzMyQbz>);
        /// `mode = "description"`.
        #[qinvokable]
        fn open_description(self: Pin<&mut QbzMyQbz>);
        /// `mode = "delete"`.
        #[qinvokable]
        fn open_delete(self: Pin<&mut QbzMyQbz>);
        #[qinvokable]
        fn upload_cover(self: Pin<&mut QbzMyQbz>);
        #[qinvokable]
        fn remove_cover(self: Pin<&mut QbzMyQbz>);
        /// Reads the current mode from the Rust document, not from QML.
        #[qinvokable]
        fn toggle_play_mode(self: Pin<&mut QbzMyQbz>);
        /// mixtape ⇄ collection; any other current kind is refused with a toast.
        #[qinvokable]
        fn convert_kind(self: Pin<&mut QbzMyQbz>);

        // --- Edit modal submits ---------------------------------------------

        /// Trim → an empty name closes without writing.
        #[qinvokable]
        fn edit_submit_rename(self: Pin<&mut QbzMyQbz>, name: QString);
        /// Trimmed-empty → SQL NULL.
        #[qinvokable]
        fn edit_submit_description(self: Pin<&mut QbzMyQbz>, description: QString);
        #[qinvokable]
        fn edit_confirm_delete(self: Pin<&mut QbzMyQbz>);
        #[qinvokable]
        fn edit_close(self: Pin<&mut QbzMyQbz>);

        // --- DJ-mix modal ---------------------------------------------------

        /// Clamped to 0..len-1 by the controller.
        #[qinvokable]
        fn mix_set_index(self: Pin<&mut QbzMyQbz>, index: i32);
        /// Confirm: dedup + hybrid sample + replace the queue.
        #[qinvokable]
        fn mix_shuffle(self: Pin<&mut QbzMyQbz>);
        #[qinvokable]
        fn mix_close(self: Pin<&mut QbzMyQbz>);

        // --- Nav branding ---------------------------------------------------

        /// Trimmed-empty coerces to "My QBZ" and PERSISTS that literal
        /// (myqbz_prefs.rs:121-128).
        #[qinvokable]
        fn branding_set_label(self: Pin<&mut QbzMyQbz>, label: QString);
        /// rfd picker (svg png jpg jpeg webp). Cancel ⇒ no-op, no toast.
        #[qinvokable]
        fn branding_pick_icon(self: Pin<&mut QbzMyQbz>);
        /// Clears the stored path (does NOT store a default path).
        #[qinvokable]
        fn branding_reset_icon(self: Pin<&mut QbzMyQbz>);

        // --- Detail: the row-patch channel ----------------------------------

        /// PARTIAL row updates for the detail list, as
        /// `{ "id": <collectionId>, "rows": [ { "position", "sourceItemId",
        /// ...only the changed fields } ] }`. The QML host `Object.assign`s
        /// each entry onto the live row object it finds by `position` and bumps
        /// its `patchRev` once for the whole batch.
        ///
        /// This is the ONE `#[qsignal]` in this bridge and the module header's
        /// "every state change is a property republish" has a stated exception
        /// here, for the same reason `QbzHome.foryouArtReady` exists: a
        /// per-item republish of a whole document is quadratic. `apply_resolved`
        /// used to clone + serialise the ENTIRE `DetailDoc` per resolved item
        /// (200 serialize -> parse -> patch cycles on the GUI thread for a big
        /// artist_collection) and `patch_expanded` did it while the document
        /// carried every loaded row's `inlineTracks`.
        ///
        /// A SIGNAL rather than an eighth `#[qproperty]`: a property setter
        /// compares before emitting, so two identical consecutive deltas would
        /// drop the second. And because a signal is a delta, a view mounted
        /// after it has missed it — `detail_resync` closes that hole.
        ///
        /// `#[auto_cxx_name]` makes the QML spelling `detailRowsPatched`, i.e.
        /// the handler is `onDetailRowsPatched(patchJson)`.
        #[qsignal]
        fn detail_rows_patched(self: Pin<&mut QbzMyQbz>, patch_json: QString);
    }

    impl cxx_qt::Threading for QbzMyQbz {}
}

use qbz_myqbz_bridge::QbzMyQbz;

/// Rust side of the MyQBZ bridge (plain storage, phase-1 pattern).
pub struct QbzMyQbzRust {
    mixtapes_json: QString,
    collections_json: QString,
    detail_json: QString,
    create_json: QString,
    edit_json: QString,
    mix_json: QString,
    branding_json: QString,
}

impl Default for QbzMyQbzRust {
    fn default() -> Self {
        Self {
            // Every document defaults to a PARSEABLE "{}" so QML's JSON.parse
            // never throws on frame 1 (home_bridge.rs:250-257).
            mixtapes_json: QString::from("{}"),
            collections_json: QString::from("{}"),
            detail_json: QString::from("{}"),
            create_json: QString::from("{}"),
            edit_json: QString::from("{}"),
            mix_json: QString::from("{}"),
            // Seeded at CONSTRUCTION, not at boot(): the nav catalog binds the
            // section label on the very first frame. Before session activation
            // this is the "My QBZ" default document.
            branding_json: QString::from(crate::myqbz_prefs_qt::branding_json().as_str()),
        }
    }
}

// ---------------------------------------------------------------------------
// The UI hop (bridges/mod.rs pattern)
// ---------------------------------------------------------------------------

static QT_THREAD: OnceLock<CxxQtThread<QbzMyQbz>> = OnceLock::new();

/// Queue a MyQBZ-bridge mutation onto the Qt event loop (no-op before boot
/// registers the thread).
pub(crate) fn ui(f: impl FnOnce(Pin<&mut QbzMyQbz>) + Send + 'static) {
    if let Some(thread) = QT_THREAD.get() {
        let _ = thread.queue(f);
    }
}

impl qbz_myqbz_bridge::QbzMyQbz {
    pub fn boot(self: Pin<&mut Self>) {
        if QT_THREAD.set(self.qt_thread()).is_err() {
            log::warn!("[qbz-qt] myqbz Qt thread already registered");
        }
        // The grids are lazy per visit (navigate_to), the detail is opened by
        // id and branding is already seeded, so there is nothing to publish
        // here.
    }

    // --- Grids -------------------------------------------------------------

    pub fn open_card(self: Pin<&mut Self>, id: QString) {
        crate::myqbz_detail_qt::open(id.to_string());
    }

    pub fn set_collection_hidden(self: Pin<&mut Self>, id: QString, hidden: bool) {
        crate::myqbz_qt::set_collection_hidden(id.to_string(), hidden);
    }

    pub fn kiosk_grid_artwork(self: Pin<&mut Self>, grid: QString, first: i32, last: i32, px: i32) {
        crate::myqbz_qt::kiosk_grid_artwork(&grid.to_string(), first, last, px);
    }

    pub fn kiosk_detail_artwork(self: Pin<&mut Self>, first: i32, last: i32, px: i32) {
        crate::myqbz_detail_qt::kiosk_detail_artwork(first, last, px);
    }

    pub fn grid_search(self: Pin<&mut Self>, grid: QString, query: QString) {
        crate::myqbz_qt::grid_search(&grid.to_string(), &query.to_string());
    }

    pub fn grid_set_sort(self: Pin<&mut Self>, grid: QString, field: QString) {
        crate::myqbz_qt::grid_set_sort(&grid.to_string(), &field.to_string());
    }

    pub fn grid_set_view(self: Pin<&mut Self>, grid: QString, view: QString) {
        crate::myqbz_qt::grid_set_view(&grid.to_string(), &view.to_string());
    }

    pub fn grid_set_kind_filter(self: Pin<&mut Self>, kind: QString) {
        crate::myqbz_qt::grid_set_kind_filter(&kind.to_string());
    }

    pub fn grid_reset(self: Pin<&mut Self>, grid: QString) {
        crate::myqbz_qt::grid_reset(&grid.to_string());
    }

    // --- Create modal ------------------------------------------------------

    pub fn create_open(self: Pin<&mut Self>, kind: QString) {
        crate::myqbz_qt::create_open(&kind.to_string());
    }

    pub fn create_set_kind(self: Pin<&mut Self>, kind: QString) {
        crate::myqbz_qt::create_set_kind(&kind.to_string());
    }

    pub fn create_submit(self: Pin<&mut Self>, name: QString) {
        crate::myqbz_qt::create_submit(name.to_string());
    }

    pub fn create_close(self: Pin<&mut Self>) {
        crate::myqbz_qt::create_close();
    }

    // --- Detail: toolbar ---------------------------------------------------

    pub fn detail_search(self: Pin<&mut Self>, query: QString) {
        crate::myqbz_detail_qt::search(&query.to_string());
    }

    pub fn detail_set_sort(self: Pin<&mut Self>, field: QString) {
        crate::myqbz_detail_qt::set_sort(&field.to_string());
    }

    pub fn detail_set_type_filter(self: Pin<&mut Self>, value: QString) {
        crate::myqbz_detail_qt::set_type_filter(&value.to_string());
    }

    pub fn detail_toggle_source_filter(self: Pin<&mut Self>, kind: QString) {
        crate::myqbz_detail_qt::toggle_source_filter(&kind.to_string());
    }

    pub fn detail_set_view_mode(self: Pin<&mut Self>, mode: QString) {
        crate::myqbz_detail_qt::set_view_mode(&mode.to_string());
    }

    pub fn detail_toggle_row_expand(self: Pin<&mut Self>, source_item_id: QString) {
        crate::myqbz_detail_qt::toggle_row_expand(&source_item_id.to_string());
    }

    pub fn detail_resync(self: Pin<&mut Self>) {
        crate::myqbz_detail_qt::resync();
    }

    pub fn detail_toggle_select_mode(self: Pin<&mut Self>) {
        crate::myqbz_detail_qt::toggle_select_mode();
    }

    pub fn detail_toggle_item_select(self: Pin<&mut Self>, position: i32, shift: bool) {
        crate::myqbz_detail_qt::toggle_item_select(position, shift);
    }

    pub fn detail_reset_filters(self: Pin<&mut Self>) {
        crate::myqbz_detail_qt::reset_filters();
    }

    // --- Detail: navigation out of a row -----------------------------------

    pub fn open_item(
        self: Pin<&mut Self>,
        source: QString,
        item_type: QString,
        source_item_id: QString,
    ) {
        crate::myqbz_detail_qt::open_item(
            source.to_string(),
            item_type.to_string(),
            source_item_id.to_string(),
        );
    }

    pub fn open_artist(
        self: Pin<&mut Self>,
        source: QString,
        artist_name: QString,
        artist_id: QString,
    ) {
        crate::myqbz_detail_qt::open_artist(
            source.to_string(),
            artist_name.to_string(),
            artist_id.to_string(),
        );
    }

    // --- Detail: hero actions ----------------------------------------------

    pub fn play_all(self: Pin<&mut Self>) {
        crate::myqbz_play_qt::play_all();
    }

    pub fn shuffle(self: Pin<&mut Self>) {
        crate::myqbz_play_qt::shuffle();
    }

    pub fn dj_mix(self: Pin<&mut Self>) {
        crate::myqbz_mix_qt::open();
    }

    pub fn sync(self: Pin<&mut Self>) {
        // 1:1 with main.rs:6386-6392 — the button exists, the command does not.
        log::info!(
            "[qbz-qt] myqbz sync requested: artist_discography is a marker only, \
             there is no sync command (1:1 with the Slint no-op)"
        );
    }

    // --- Detail: per-row actions -------------------------------------------

    pub fn play_item(self: Pin<&mut Self>, source_item_id: QString) {
        crate::myqbz_play_qt::play_item(source_item_id.to_string());
    }

    pub fn item_action(self: Pin<&mut Self>, source_item_id: QString, action: QString) {
        crate::myqbz_play_qt::item_action(source_item_id.to_string(), action.to_string());
    }

    pub fn remove_item(self: Pin<&mut Self>, position: i32) {
        crate::myqbz_edit_qt::remove_item(position);
    }

    pub fn bulk_action(self: Pin<&mut Self>, id: QString) {
        crate::myqbz_detail_qt::bulk_action(id.to_string());
    }

    pub fn ensure_expanded(self: Pin<&mut Self>) {
        crate::myqbz_detail_qt::ensure_expanded();
    }

    pub fn inline_track_action(
        self: Pin<&mut Self>,
        item_source_item_id: QString,
        track_id: QString,
        action: QString,
    ) {
        crate::myqbz_play_qt::play_inline_track(
            item_source_item_id.to_string(),
            track_id.to_string(),
            action.to_string(),
        );
    }

    // --- Detail: overflow menu → edit modal / cover -------------------------

    pub fn open_rename(self: Pin<&mut Self>) {
        crate::myqbz_edit_qt::open_rename();
    }

    pub fn open_description(self: Pin<&mut Self>) {
        crate::myqbz_edit_qt::open_description();
    }

    pub fn open_delete(self: Pin<&mut Self>) {
        crate::myqbz_edit_qt::open_delete();
    }

    pub fn upload_cover(self: Pin<&mut Self>) {
        crate::myqbz_cover_qt::upload();
    }

    pub fn remove_cover(self: Pin<&mut Self>) {
        crate::myqbz_cover_qt::remove();
    }

    pub fn toggle_play_mode(self: Pin<&mut Self>) {
        crate::myqbz_edit_qt::toggle_play_mode();
    }

    pub fn convert_kind(self: Pin<&mut Self>) {
        crate::myqbz_edit_qt::convert_kind();
    }

    // --- Edit modal submits ------------------------------------------------

    pub fn edit_submit_rename(self: Pin<&mut Self>, name: QString) {
        crate::myqbz_edit_qt::submit_rename(name.to_string());
    }

    pub fn edit_submit_description(self: Pin<&mut Self>, description: QString) {
        crate::myqbz_edit_qt::submit_description(description.to_string());
    }

    pub fn edit_confirm_delete(self: Pin<&mut Self>) {
        crate::myqbz_edit_qt::confirm_delete();
    }

    pub fn edit_close(self: Pin<&mut Self>) {
        crate::myqbz_edit_qt::close();
    }

    // --- DJ-mix modal ------------------------------------------------------

    pub fn mix_set_index(self: Pin<&mut Self>, index: i32) {
        crate::myqbz_mix_qt::apply_index(index);
    }

    pub fn mix_shuffle(self: Pin<&mut Self>) {
        crate::myqbz_mix_qt::confirm();
    }

    pub fn mix_close(self: Pin<&mut Self>) {
        crate::myqbz_mix_qt::close();
    }

    // --- Nav branding ------------------------------------------------------

    pub fn branding_set_label(self: Pin<&mut Self>, label: QString) {
        crate::myqbz_prefs_qt::set_label(&label.to_string());
    }

    pub fn branding_pick_icon(self: Pin<&mut Self>) {
        crate::myqbz_prefs_qt::pick_icon();
    }

    pub fn branding_reset_icon(self: Pin<&mut Self>) {
        crate::myqbz_prefs_qt::reset_icon();
    }
}
