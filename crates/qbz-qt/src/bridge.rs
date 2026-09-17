//! The shared Qt-side bridge object for cross-domain shell state.
//!
//! One `QbzBridge` QObject is registered as a QML SINGLETON (`QbzBridge.*`
//! in QML). All session/login/offline/shell state the QML needs lives in
//! its properties; user actions come in as invokables. Invokable bodies
//! NEVER block the Qt thread — they enqueue work onto the process-global
//! tokio runtime (see `main.rs`) and the async results hop back here
//! through `CxxQtThread::queue` (the cxx-qt analogue of Slint's
//! `upgrade_in_event_loop`).
//!
//! `#[auto_cxx_name]` on the extern blocks keeps Rust names snake_case
//! while QML/C++ see camelCase (`login_phase` -> `loginPhase`), matching
//! the property names of the Slint `LoginState`/`OfflineState` globals.

#[cxx_qt::bridge]
pub mod qbz_bridge {
    extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        // --- Album / Artist detail views: MOVED (phase 23) ----------------
        // album_* -> QbzAlbum (album_bridge.rs), artist_* + the releases
        // "Load more" signal -> QbzArtist (artist_bridge.rs). `view_param_id`
        // was deleted, not moved: write-only, zero readers — the rationale
        // lives in artist_bridge.rs's header.

        // --- Settings (phase 10) -----------------------------------------
        // One JSON document (settings_qt.rs SettingsDoc: audio + playback
        // row states, select option lists, output-device groups).
        #[qproperty(QString, settings_json)]
        // The ACTIVE Settings sub-section (0 Audio … 8 Flatpak/Snap — the
        // display order is documented in settings/SettingsView.qml's header).
        //
        // Bridge state rather than QML state, and that is load-bearing: the
        // content Loader UNMOUNTS SettingsView when the user navigates away,
        // which destroys a local `property int section` along with it. The
        // Blacklist panel's "Manage" chevron does exactly that — it opens the
        // manager view — so on Back the user landed on Audio instead of
        // Blacklist. This is Slint's `SettingsState.section`, which is a global
        // for the same reason (SettingsView.slint:121-200).
        #[qproperty(i32, settings_section)]
        // --- Search: MOVED (cortinilla-parity contract C0) -----------------
        // cortinilla_* + search_json + intelligent_search -> QbzSearch
        // (search_bridge.rs). `selected_index` was RENAMED on the way out to
        // `cortinilla_selected_index` (it is cortinilla-only state, and the
        // bare name on an app-wide singleton is a collision hazard), and
        // `cortinilla_scroll_y` went f32 -> f64 to match the immersive twin.

        // --- Playlist view (phase 17) --------------------------------------
        // ONE JSON document (playlist_qt.rs PlaylistDoc: header + track
        // rows + ownership/follow/pin/sort/search state).
        #[qproperty(QString, playlist_json)]
        // --- Filter by genre (shared, per context) -------------------------
        // ONE JSON document (genre_filter_qt.rs FilterDoc): the popup model
        // (chips / tree / remember / advanced / loading) PLUS the per-context
        // selection counts and selected genre NAMES. Every surface that draws
        // a genre button reads this one property: Discover (context
        // "discover", server-side via get_discover_index) and Library > All
        // (context "library-all", client-side over the feed).
        #[qproperty(QString, genre_filter_json)]
        // --- Discover section configurator (the toolbar gear) --------------
        // ONE JSON document (discover_config_qt.rs ConfigDoc: the active
        // tab's ordered rows + enabled/total counts).
        #[qproperty(QString, discover_config_json)]
        type QbzBridge = super::QbzBridgeRust;

        /// TEMP (phase 23 split): registers the QbzBridge Qt-thread
        /// hop while the remaining domains still live here. Removed
        /// when the last domain moves out.
        #[qinvokable]
        fn boot(self: Pin<&mut QbzBridge>);

        // --- Settings (phase 10) ------------------------------------------
        /// Toggle rows (settings_qt.rs key dispatch; persists + applies +
        /// republishes `settingsJson`).
        #[qinvokable]
        fn settings_bool(self: Pin<&mut QbzBridge>, key: QString, value: bool);
        /// Select rows: the picked OPTION INDEX within the row's list.
        #[qinvokable]
        fn settings_select(self: Pin<&mut QbzBridge>, key: QString, index: i32);
        #[qinvokable]
        fn settings_pick_background_image(self: Pin<&mut QbzBridge>);
        #[qinvokable]
        fn settings_clear_background_image(self: Pin<&mut QbzBridge>);
        /// Slider rows (initial buffer size).
        #[qinvokable]
        fn settings_slider(self: Pin<&mut QbzBridge>, key: QString, value: i32);
        /// Free-text rows (Qobuz Connect device name).
        #[qinvokable]
        fn settings_string(self: Pin<&mut QbzBridge>, key: QString, value: QString);
        /// Settings sub-navigation: switch the active section.
        ///
        /// The rows MUST call this rather than assigning `section` in QML.
        /// `section` is a BINDING onto `settingsSection`, and in QML an
        /// imperative assignment silently destroys the binding it lands on — so
        /// one stray `root.section = n` would work for exactly one click and
        /// then strand the view on stale local state.
        #[qinvokable]
        fn settings_set_section(self: Pin<&mut QbzBridge>, index: i32);
        /// "Reset to defaults" (audio + playback stores, quality pref).
        #[qinvokable]
        fn settings_reset(self: Pin<&mut QbzBridge>);
        /// Output-device refresh button: release a held device, re-enumerate.
        #[qinvokable]
        fn refresh_devices(self: Pin<&mut QbzBridge>);
        /// Settings > Network > "Test connection": persists every staged
        /// field (host/port/kind/auth/username, and password unless blank —
        /// blank means "keep the saved one"), applies it, and tests
        /// reachability. Republishes `settingsJson` with the busy/result
        /// state under `doc.network` when it's done.
        #[qinvokable]
        fn network_test_and_save(
            self: Pin<&mut QbzBridge>,
            kind_index: i32,
            host: QString,
            port: i32,
            auth_enabled: bool,
            username: QString,
            password: QString,
            insecure_tls: bool,
        );

        // --- Integrations (phase 19) ---------------------------------------
        /// Non-toggle integration actions (integrations_qt.rs): Last.fm
        /// connect/open-auth-url/finish/disconnect, ListenBrainz disconnect.
        #[qinvokable]
        fn integrations_action(self: Pin<&mut QbzBridge>, action: QString);

        // --- Search: MOVED (cortinilla-parity contract C0) -----------------
        // search_live / search_submit / cortinilla_* / search_tab_changed /
        // search_load_more / search_filter_changed / toggle_intelligent_search
        // -> QbzSearch (search_bridge.rs).

        // --- Playlist view (phase 17) --------------------------------------
        /// Sidebar playlist row / playlist card click: open the detail view.
        #[qinvokable]
        fn open_playlist(self: Pin<&mut QbzBridge>, playlist_id: QString);
        #[qinvokable]
        fn playlist_play_all(self: Pin<&mut QbzBridge>);
        #[qinvokable]
        fn playlist_shuffle(self: Pin<&mut QbzBridge>);
        /// Header context-menu queue actions for the OPEN playlist. Unlike
        /// the card-level by-id seam, this also serves local and mixed
        /// playlists from their already-resolved queue snapshot.
        #[qinvokable]
        fn playlist_enqueue_all(self: Pin<&mut QbzBridge>, mode: QString);
        #[qinvokable]
        fn playlist_toggle_favorite(self: Pin<&mut QbzBridge>);
        #[qinvokable]
        fn playlist_toggle_follow(self: Pin<&mut QbzBridge>);
        #[qinvokable]
        fn playlist_toggle_pin(self: Pin<&mut QbzBridge>);

        /// Playlist header cover menu (cover_artwork_qt.rs): pick / clear
        /// the custom cover override for the OPEN playlist.
        #[qinvokable]
        fn playlist_cover_add(self: Pin<&mut QbzBridge>, playlist_id: QString);
        #[qinvokable]
        fn playlist_cover_remove(self: Pin<&mut QbzBridge>, playlist_id: QString);
        /// "Copy to your library" (foreign playlists).
        #[qinvokable]
        fn playlist_copy(self: Pin<&mut QbzBridge>);
        /// Copy the public Qobuz playlist URL. Hidden for local playlists.
        #[qinvokable]
        fn playlist_share(self: Pin<&mut QbzBridge>, playlist_id: QString);
        #[qinvokable]
        fn playlist_rename(self: Pin<&mut QbzBridge>, name: QString);
        #[qinvokable]
        fn playlist_delete(self: Pin<&mut QbzBridge>);
        #[qinvokable]
        fn playlist_set_sort(self: Pin<&mut QbzBridge>, field: QString);
        #[qinvokable]
        fn playlist_set_search(self: Pin<&mut QbzBridge>, query: QString);
        /// Row play / row ⋯ queueing / owner Remove-from-playlist / drag
        /// reorder.
        #[qinvokable]
        fn playlist_play_track(self: Pin<&mut QbzBridge>, track_id: QString);
        #[qinvokable]
        fn playlist_enqueue_track(self: Pin<&mut QbzBridge>, track_id: QString, mode: QString);
        /// "Remove from playlist" by DISPLAY row id (`item.id`). A string,
        /// not the numeric membership id: a LOCAL playlist's rows carry
        /// library-row / `"plex:<key>"` / path ids too, and the numeric
        /// invokable this replaced could not express them.
        #[qinvokable]
        fn playlist_remove_track(self: Pin<&mut QbzBridge>, row_id: QString);
        #[qinvokable]
        fn playlist_remove_tracks(self: Pin<&mut QbzBridge>, ids_json: QString);
        /// Drag-reorder drop: visible row `from` -> insertion slot `slot`.
        #[qinvokable]
        fn playlist_reorder(self: Pin<&mut QbzBridge>, from: i32, slot: i32);
        /// Per-row reorder chevrons: -1 = up, +1 = down.
        #[qinvokable]
        fn playlist_move_row(self: Pin<&mut QbzBridge>, row_id: QString, delta: i32);

        /// Card-level playlist actions (LibPlaylistCard overlay + menu).
        #[qinvokable]
        fn playlist_set_follow_by_id(self: Pin<&mut QbzBridge>, playlist_id: QString, follow: bool);

        // --- Filter by genre -----------------------------------------------
        /// Genre button click: switch the edited context ("discover" |
        /// "library-all"), publish the popup model, lazy-load the parent
        /// genres on first open.
        #[qinvokable]
        fn genre_open(self: Pin<&mut QbzBridge>, context: QString);
        /// Chip / tree-row click: flip the id in the CURRENT context, persist,
        /// and refresh that context's consumer.
        #[qinvokable]
        fn genre_toggle(self: Pin<&mut QbzBridge>, genre_id: QString);
        /// Advanced tree chevron: expand/collapse + lazy-load that level.
        #[qinvokable]
        fn genre_toggle_expand(self: Pin<&mut QbzBridge>, genre_id: QString);
        /// Advanced-view search box (filters the loaded genres, flat).
        #[qinvokable]
        fn genre_search(self: Pin<&mut QbzBridge>, query: QString);
        /// "Clear filter" — drops the CURRENT context's selection.
        #[qinvokable]
        fn genre_clear(self: Pin<&mut QbzBridge>);
        /// "Remember selection" toggle (off also deletes the persisted file).
        #[qinvokable]
        fn genre_set_remember(self: Pin<&mut QbzBridge>, value: bool);
        /// "Advanced view" toggle (eager-loads every parent's children).
        #[qinvokable]
        fn genre_set_advanced(self: Pin<&mut QbzBridge>, value: bool);

        // --- Discover section configurator ---------------------------------
        /// Gear click: publish the rows for the tab the modal will show.
        #[qinvokable]
        fn discover_config_open(self: Pin<&mut QbzBridge>, tab: QString);
        /// Row click / checkbox: show or hide that section on that tab.
        #[qinvokable]
        fn discover_toggle_section(self: Pin<&mut QbzBridge>, tab: QString, section_id: QString);
        /// Reorder chevrons: -1 up, +1 down (clamped at the boundaries).
        #[qinvokable]
        fn discover_move_section(
            self: Pin<&mut QbzBridge>,
            tab: QString,
            section_id: QString,
            dir: i32,
        );
        /// "Reset to defaults" — that tab only.
        #[qinvokable]
        fn discover_reset_tab(self: Pin<&mut QbzBridge>, tab: QString);
    }

    impl cxx_qt::Threading for QbzBridge {}
}

use core::pin::Pin;

use cxx_qt::Threading as _;
use cxx_qt_lib::QString;

/// Rust side of the bridge. All fields are driven exclusively through the
/// generated `set_*` methods on the Qt thread; the struct itself is plain
/// storage (as required by cxx-qt's Default-constructed qobjects).
pub struct QbzBridgeRust {
    settings_json: QString,
    settings_section: i32,
    playlist_json: QString,
    genre_filter_json: QString,
    discover_config_json: QString,
}

impl Default for QbzBridgeRust {
    fn default() -> Self {
        Self {
            // The Local Library order is boot-critical: NavFlyout and a
            // logged-off shell can mount before the async full settings
            // snapshot. Seed that global pref synchronously so their first
            // frame already has the user's real landing tab.
            settings_json: QString::from(crate::settings_qt::settings_seed_json().as_str()),
            // 0 = Audio, the section Settings opens on.
            settings_section: 0,
            playlist_json: QString::from("{}"),
            // Seeded from the PERSISTED selection (no network): a remembered
            // filter colors both genre buttons and narrows the Library feed
            // before the popup has ever been opened.
            genre_filter_json: QString::from(crate::genre_filter_qt::current_json().as_str()),
            // Seeded for the tab Discover mounts on.
            discover_config_json: QString::from(
                crate::discover_config_qt::rows_json("home").as_str(),
            ),
        }
    }
}

impl qbz_bridge::QbzBridge {
    pub fn boot(self: Pin<&mut Self>) {
        crate::register_qt_thread(self.qt_thread());
    }

    pub fn integrations_action(self: Pin<&mut Self>, action: QString) {
        crate::integrations_action(action.to_string());
    }

    pub fn settings_bool(self: Pin<&mut Self>, key: QString, value: bool) {
        crate::settings_bool(key.to_string(), value);
    }

    pub fn settings_pick_background_image(self: Pin<&mut Self>) {
        crate::settings_qt::pick_background_image();
    }

    pub fn settings_clear_background_image(self: Pin<&mut Self>) {
        crate::settings_qt::clear_background_image();
    }

    pub fn settings_select(self: Pin<&mut Self>, key: QString, index: i32) {
        crate::settings_select(key.to_string(), index);
    }

    pub fn settings_slider(self: Pin<&mut Self>, key: QString, value: i32) {
        crate::settings_slider(key.to_string(), value);
    }

    pub fn settings_string(self: Pin<&mut Self>, key: QString, value: QString) {
        crate::settings_string(key.to_string(), value.to_string());
    }

    /// Bridge-local state with one reader outside: the section only has to
    /// survive a Loader unmount WITHIN a session (Settings opens on Audio),
    /// and it rides to page_restore_qt so "Where you left off" reopens
    /// Settings on the same section.
    pub fn settings_set_section(mut self: Pin<&mut Self>, index: i32) {
        if index == 11 && !crate::orbit_qt::enabled() { return; }
        crate::page_restore_qt::note_args("settings", serde_json::json!({ "section": index }));
        self.as_mut().set_settings_section(index);
    }

    pub fn settings_reset(self: Pin<&mut Self>) {
        crate::settings_reset();
    }

    pub fn refresh_devices(self: Pin<&mut Self>) {
        crate::refresh_devices();
    }

    pub fn network_test_and_save(
        self: Pin<&mut Self>,
        kind_index: i32,
        host: QString,
        port: i32,
        auth_enabled: bool,
        username: QString,
        password: QString,
        insecure_tls: bool,
    ) {
        crate::network_test_and_save(
            kind_index,
            host.to_string(),
            port,
            auth_enabled,
            username.to_string(),
            password.to_string(),
            insecure_tls,
        );
    }

    pub fn open_playlist(self: Pin<&mut Self>, playlist_id: QString) {
        crate::open_playlist(playlist_id.to_string());
    }

    pub fn playlist_play_all(self: Pin<&mut Self>) {
        crate::playlist_play_all();
    }

    pub fn playlist_shuffle(self: Pin<&mut Self>) {
        crate::playlist_shuffle();
    }

    pub fn playlist_enqueue_all(self: Pin<&mut Self>, mode: QString) {
        crate::playlist_enqueue_all(mode.to_string());
    }

    pub fn playlist_toggle_favorite(self: Pin<&mut Self>) {
        crate::playlist_qt::toggle_favorite();
    }

    pub fn playlist_toggle_follow(self: Pin<&mut Self>) {
        crate::playlist_toggle_follow();
    }

    pub fn playlist_toggle_pin(self: Pin<&mut Self>) {
        crate::playlist_qt::toggle_pin();
    }

    pub fn playlist_cover_add(self: Pin<&mut Self>, playlist_id: QString) {
        crate::cover_artwork_qt::add_custom_playlist_cover(playlist_id.to_string());
    }
    pub fn playlist_cover_remove(self: Pin<&mut Self>, playlist_id: QString) {
        crate::cover_artwork_qt::remove_custom_playlist_cover(playlist_id.to_string());
    }

    pub fn playlist_copy(self: Pin<&mut Self>) {
        crate::playlist_copy();
    }

    pub fn playlist_share(self: Pin<&mut Self>, playlist_id: QString) {
        crate::share_qt::share_playlist(playlist_id.to_string());
    }

    pub fn playlist_rename(self: Pin<&mut Self>, name: QString) {
        crate::playlist_rename(name.to_string());
    }

    pub fn playlist_delete(self: Pin<&mut Self>) {
        crate::playlist_delete();
    }

    pub fn playlist_set_sort(self: Pin<&mut Self>, field: QString) {
        crate::playlist_qt::set_sort(&field.to_string());
    }

    pub fn playlist_set_search(self: Pin<&mut Self>, query: QString) {
        crate::playlist_qt::set_search(&query.to_string());
    }

    pub fn playlist_play_track(self: Pin<&mut Self>, track_id: QString) {
        crate::playlist_play_track(track_id.to_string());
    }

    pub fn playlist_enqueue_track(self: Pin<&mut Self>, track_id: QString, mode: QString) {
        crate::playlist_enqueue_track(track_id.to_string(), mode.to_string());
    }

    pub fn playlist_remove_track(self: Pin<&mut Self>, row_id: QString) {
        crate::playlist_remove_track(row_id.to_string());
    }

    pub fn playlist_remove_tracks(self: Pin<&mut Self>, ids_json: QString) {
        crate::playlist_remove_tracks(ids_json.to_string());
    }

    pub fn playlist_reorder(self: Pin<&mut Self>, from: i32, slot: i32) {
        crate::playlist_reorder(from, slot);
    }

    pub fn playlist_move_row(self: Pin<&mut Self>, row_id: QString, delta: i32) {
        crate::playlist_move_row(row_id.to_string(), delta);
    }

    pub fn playlist_set_follow_by_id(self: Pin<&mut Self>, playlist_id: QString, follow: bool) {
        if let Ok(pid) = playlist_id.to_string().parse::<u64>() {
            crate::playlist_set_follow_by_id(pid, follow);
        }
    }

    pub fn genre_open(self: Pin<&mut Self>, context: QString) {
        crate::genre_filter_qt::open(&context.to_string());
    }

    pub fn genre_toggle(self: Pin<&mut Self>, genre_id: QString) {
        crate::genre_filter_qt::toggle(&genre_id.to_string());
    }

    pub fn genre_toggle_expand(self: Pin<&mut Self>, genre_id: QString) {
        crate::genre_filter_qt::toggle_expand(&genre_id.to_string());
    }

    pub fn genre_search(self: Pin<&mut Self>, query: QString) {
        crate::genre_filter_qt::set_search(&query.to_string());
    }

    pub fn genre_clear(self: Pin<&mut Self>) {
        crate::genre_filter_qt::clear();
    }

    pub fn genre_set_remember(self: Pin<&mut Self>, value: bool) {
        crate::genre_filter_qt::set_remember(value);
    }

    pub fn genre_set_advanced(self: Pin<&mut Self>, value: bool) {
        crate::genre_filter_qt::set_advanced(value);
    }

    pub fn discover_config_open(self: Pin<&mut Self>, tab: QString) {
        crate::discover_config_qt::open(&tab.to_string());
    }

    pub fn discover_toggle_section(self: Pin<&mut Self>, tab: QString, section_id: QString) {
        crate::discover_config_qt::toggle_section(&tab.to_string(), &section_id.to_string());
    }

    pub fn discover_move_section(
        self: Pin<&mut Self>,
        tab: QString,
        section_id: QString,
        dir: i32,
    ) {
        crate::discover_config_qt::move_section(&tab.to_string(), &section_id.to_string(), dir);
    }

    pub fn discover_reset_tab(self: Pin<&mut Self>, tab: QString) {
        crate::discover_config_qt::reset_tab(&tab.to_string());
    }
}
