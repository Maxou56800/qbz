//! QbzAlbum — Album detail view domain bridge (phase 23 split of the
//! QbzBridge God-object; the pattern is documented in main.rs).
//!
//! The module is `qbz_album_bridge`, not `qbz_album`: bridge modules are
//! never named after a workspace crate (the `qbz_library` collision is the
//! documented precedent). The QML type name comes from the QObject
//! (`QbzAlbum`), not from the module, so QML is unaffected.
//!
//! Props: the one album-view JSON document + its loading flag.
//! Invokable: openAlbum (nav push + fetch + publish).
//!
//! The album/artist domains are TWO singletons on purpose (owner decision):
//! one per view, so a view's state can never be perturbed by the other's
//! fetch. `view_param_id` did NOT come along — see artist_bridge.rs for the
//! deletion rationale; the id QML needs is already inside `albumJson`
//! (`AlbumHeader.id`).

use std::pin::Pin;
use std::sync::OnceLock;

use cxx_qt::CxxQtThread;
use cxx_qt::Threading as _;
use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod qbz_album_bridge {
    extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        // --- Album detail view ---------------------------------------------
        #[qproperty(bool, album_loading)]
        // ONE JSON document (album_qt.rs AlbumViewData: header + track rows
        // + works/goodies/related buckets); QML JSON.parse()s it once.
        #[qproperty(QString, album_json)]
        // App-wide Album Quick View. This is deliberately separate from
        // `album_json`: opening a card preview must not replace the document
        // owned by AlbumView or mutate navigation state.
        #[qproperty(QString, quick_view_json)]
        // --- Track Info modal (qml/shell/TrackInfoModal.qml) --------------
        #[qproperty(bool, track_info_loading)]
        #[qproperty(QString, track_info_json)]
        // --- Album Info modal (qml/shell/AlbumInfoModal.qml) --------------
        // One JSON document (album_info_qt.rs); loading + error ride their
        // own properties so the modal can show its skeleton without a parse.
        #[qproperty(bool, album_info_loading)]
        #[qproperty(QString, album_info_json)]
        #[qproperty(QString, album_info_error)]
        type QbzAlbum = super::QbzAlbumRust;

        /// Registers this object's Qt-thread hop (Main.qml boots EVERY
        /// domain singleton; only QbzSession.boot also fires crate::on_boot).
        #[qinvokable]
        fn boot(self: Pin<&mut QbzAlbum>);

        /// Open the album detail view (pushes "album" on the nav stack).
        #[qinvokable]
        fn open_album(self: Pin<&mut QbzAlbum>, album_id: QString);
        /// `open_album` with the title/artist the caller knows, so a release
        /// the catalog no longer has can still be named on its page.
        #[qinvokable]
        fn open_album_from(self: Pin<&mut QbzAlbum>, album_id: QString, title: QString, artist: QString);
        /// "Buy on Qobuz": the store page in the browser when the release is
        /// sold, a toast otherwise (store_qt).
        #[qinvokable]
        fn buy_album(self: Pin<&mut QbzAlbum>, album_id: QString);
        #[qinvokable]
        fn buy_track(self: Pin<&mut QbzAlbum>, track_id: QString);
        /// AlbumCard's picture-in-picture action. Fetches the compact album
        /// document without navigating or touching `album_json`.
        #[qinvokable]
        fn open_quick_view(self: Pin<&mut QbzAlbum>, album_id: QString);
        /// Every Quick View dismissal path (Escape, backdrop, close button,
        /// or a handoff into another global modal).
        #[qinvokable]
        fn close_quick_view(self: Pin<&mut QbzAlbum>);
        /// Playback/queue action over the exact physical rows shown by a
        /// local/Plex/Jellyfin/Subsonic Quick View. Catalog previews keep
        /// using QbzPlayer's album-context actions.
        #[qinvokable]
        fn quick_view_local_action(self: Pin<&mut QbzAlbum>, action: QString, track_id: QString);
        /// Info button on either now-playing bar: fetch + publish the Track
        /// Info document (track_info_qt.rs).
        #[qinvokable]
        fn open_track_info(self: Pin<&mut QbzAlbum>, track_id: QString);
        /// Header cassette button: open the MyQBZ picker with this album as
        /// the payload (album_qt::add_to_mixtape).
        #[qinvokable]
        fn add_to_mixtape(self: Pin<&mut QbzAlbum>, album_id: QString);
        /// Header booklet button: download the open album's booklet PDF to a
        /// user-chosen path (album_qt::download_booklet).
        #[qinvokable]
        fn download_booklet(self: Pin<&mut QbzAlbum>);
        /// Cover right-click menu (cover_artwork_qt.rs): pick / clear the
        /// custom cover override, save the artwork to disk.
        #[qinvokable]
        fn cover_add_custom(self: Pin<&mut QbzAlbum>, album_id: QString, artwork_url: QString);
        #[qinvokable]
        fn cover_remove_custom(self: Pin<&mut QbzAlbum>, album_id: QString);
        #[qinvokable]
        fn cover_save_as(
            self: Pin<&mut QbzAlbum>,
            album_id: QString,
            title: QString,
            artwork_url: QString,
        );
        /// ⋯ menu Share rows (share_qt.rs): copy the Qobuz link, or resolve
        /// UPC -> Deezer -> Album.link and copy that (async, toasts).
        #[qinvokable]
        fn share_qobuz_link(self: Pin<&mut QbzAlbum>, album_id: QString);
        #[qinvokable]
        fn share_album_link(self: Pin<&mut QbzAlbum>, album_id: QString);
        /// Track context-menu Share rows. The universal-link arm fetches the
        /// track's ISRC before resolving it through Deezer/Song.link.
        #[qinvokable]
        fn share_track_qobuz(self: Pin<&mut QbzAlbum>, track_id: QString);
        #[qinvokable]
        fn share_track_link(self: Pin<&mut QbzAlbum>, track_id: QString);
        /// Header info button: fetch + publish the Album Info (credits /
        /// review) document (album_info_qt.rs).
        #[qinvokable]
        fn open_album_info(self: Pin<&mut QbzAlbum>, album_id: QString);
        /// ⋯ menu offline rows (offline_cache_qt.rs): download the whole
        /// album / re-download its copies.
        #[qinvokable]
        fn album_cache_offline(self: Pin<&mut QbzAlbum>, album_id: QString);
        #[qinvokable]
        fn album_refresh_offline(self: Pin<&mut QbzAlbum>, album_id: QString);
        /// Multi-select bulk bar (album_qt::bulk_action): ids in visible
        /// order, action id from the bar's vocabulary.
        #[qinvokable]
        fn album_bulk_action(
            self: Pin<&mut QbzAlbum>,
            album_id: QString,
            ids_json: QString,
            action: QString,
        );
    }

    impl cxx_qt::Threading for QbzAlbum {}
}

use qbz_album_bridge::QbzAlbum;

/// Rust side of the album bridge (plain storage, phase-1 pattern).
pub struct QbzAlbumRust {
    album_loading: bool,
    album_json: QString,
    quick_view_json: QString,
    track_info_loading: bool,
    track_info_json: QString,
    album_info_loading: bool,
    album_info_json: QString,
    album_info_error: QString,
}

impl Default for QbzAlbumRust {
    fn default() -> Self {
        Self {
            album_loading: false,
            album_json: QString::from("{}"),
            quick_view_json: QString::from("{}"),
            track_info_loading: false,
            track_info_json: QString::from("{}"),
            album_info_loading: false,
            album_info_json: QString::from("{}"),
            album_info_error: QString::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// The UI hop (bridges/mod.rs pattern)
// ---------------------------------------------------------------------------

static QT_THREAD: OnceLock<CxxQtThread<QbzAlbum>> = OnceLock::new();

/// Queue an album-bridge mutation onto the Qt event loop (no-op before
/// boot registers the thread).
pub(crate) fn ui(f: impl FnOnce(Pin<&mut QbzAlbum>) + Send + 'static) {
    if let Some(thread) = QT_THREAD.get() {
        let _ = thread.queue(f);
    }
}

impl qbz_album_bridge::QbzAlbum {
    pub fn boot(self: Pin<&mut Self>) {
        if QT_THREAD.set(self.qt_thread()).is_err() {
            log::warn!("[qbz-qt] album Qt thread already registered");
        }
    }

    pub fn open_album(self: Pin<&mut Self>, album_id: QString) {
        crate::open_album(album_id.to_string());
    }
    pub fn open_album_from(
        self: Pin<&mut Self>,
        album_id: QString,
        title: QString,
        artist: QString,
    ) {
        crate::open_album_from(album_id.to_string(), title.to_string(), artist.to_string());
    }
    pub fn buy_album(self: Pin<&mut Self>, album_id: QString) {
        crate::store_qt::buy_album(album_id.to_string());
    }
    pub fn buy_track(self: Pin<&mut Self>, track_id: QString) {
        crate::store_qt::buy_track(track_id.to_string());
    }
    pub fn open_quick_view(self: Pin<&mut Self>, album_id: QString) {
        crate::album_quick_view_qt::open(album_id.to_string());
    }
    pub fn close_quick_view(self: Pin<&mut Self>) {
        crate::album_quick_view_qt::close();
    }
    pub fn quick_view_local_action(self: Pin<&mut Self>, action: QString, track_id: QString) {
        crate::album_quick_view_qt::local_action(action.to_string(), track_id.to_string());
    }
    pub fn open_track_info(self: Pin<&mut Self>, track_id: QString) {
        crate::track_info_qt::open(track_id.to_string());
    }
    pub fn add_to_mixtape(self: Pin<&mut Self>, album_id: QString) {
        crate::album_qt::add_to_mixtape(album_id.to_string());
    }
    pub fn download_booklet(self: Pin<&mut Self>) {
        crate::album_qt::download_booklet();
    }
    pub fn cover_add_custom(self: Pin<&mut Self>, album_id: QString, artwork_url: QString) {
        crate::cover_artwork_qt::add_custom_cover(album_id.to_string(), artwork_url.to_string());
    }
    pub fn cover_remove_custom(self: Pin<&mut Self>, album_id: QString) {
        crate::cover_artwork_qt::remove_custom_cover(album_id.to_string());
    }
    pub fn cover_save_as(
        self: Pin<&mut Self>,
        album_id: QString,
        title: QString,
        artwork_url: QString,
    ) {
        crate::cover_artwork_qt::save_cover_as(
            album_id.to_string(),
            title.to_string(),
            artwork_url.to_string(),
        );
    }
    pub fn share_qobuz_link(self: Pin<&mut Self>, album_id: QString) {
        crate::share_qt::share_album_qobuz(album_id.to_string());
    }
    pub fn share_album_link(self: Pin<&mut Self>, album_id: QString) {
        crate::share_qt::share_album_link(album_id.to_string());
    }
    pub fn share_track_qobuz(self: Pin<&mut Self>, track_id: QString) {
        crate::share_qt::share_track_qobuz(track_id.to_string());
    }
    pub fn share_track_link(self: Pin<&mut Self>, track_id: QString) {
        crate::share_qt::share_track_link(track_id.to_string());
    }
    pub fn open_album_info(self: Pin<&mut Self>, album_id: QString) {
        crate::album_info_qt::open(album_id.to_string());
    }
    pub fn album_cache_offline(self: Pin<&mut Self>, album_id: QString) {
        crate::offline_cache_qt::cache_album(album_id.to_string());
    }
    pub fn album_refresh_offline(self: Pin<&mut Self>, album_id: QString) {
        // Compatibility entry point for older QML. Whole-album access now
        // always runs the same preflight so the user chooses between repairing
        // every copy and downloading only what is missing.
        crate::offline_cache_qt::cache_album(album_id.to_string());
    }
    pub fn album_bulk_action(
        self: Pin<&mut Self>,
        album_id: QString,
        ids_json: QString,
        action: QString,
    ) {
        crate::album_qt::bulk_action(
            album_id.to_string(),
            ids_json.to_string(),
            action.to_string(),
        );
    }
}
