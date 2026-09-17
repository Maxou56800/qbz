//! QbzPlaylistEdit — the SHARED playlist editor's QML singleton (contract
//! §3.3, block 3). Port of `primitives/EditPlaylistModal.slint` plus the
//! `EditPlaylistActions` handlers in the reference's `main.rs`.
//!
//! The third of the three singletons the Playlist Manager surface owns (D2):
//! `QbzPlaylistManager` holds the manager document and the organisation
//! writes, `QbzFolderEdit` owns the two folder panels, and this one owns the
//! ONE rename / description / offline-only / delete modal — for BOTH kinds of
//! playlist (`local:<uuid>` and a Qobuz `u64`) and from every surface that
//! offers "Edit playlist":
//!
//!   * the manager's grid card, list row and tree row (block 4),
//!   * the sidebar's row context menu (block 5),
//!   * the playlist DETAIL header's pencil (block 7) — which is why
//!     `qml/views/PlaylistView.qml`'s inline rename+delete popup is deleted:
//!     it could express neither a description nor the offline-only flag, and
//!     a second editor is exactly the fork the port rule forbids.
//!
//! It is a SEPARATE singleton from `QbzPlaylist` for the same reason
//! `QbzPlaylistPicker` is: its callers live outside the detail view, and in
//! most of those sessions no playlist detail is open at all — so nothing here
//! may read the open document, and a detail republish can never perturb an
//! open editor.
//!
//! # ONE document, and the id is NOT in QML's hands
//!
//! `editJson` (§4.6) carries the seeds plus `descLoaded`, `isLocal` and
//! `busy`. The id being edited lives in RUST state and is never echoed back
//! from QML: a republish can then never make the modal save under the wrong
//! id, which is the same rule `folder_edit_qt` holds the folder id under.
//!
//! # `descLoaded` is load-bearing, not cosmetic (§5.2)
//!
//! `false` means the real description could not be resolved. The modal then
//! does NOT render the description field at all and `save` passes `None`,
//! which `update_playlist` reads as "leave it alone". A field the user never
//! saw can never overwrite stored data. The reference seeds `""` and always
//! sends `Some(trimmed)`, so renaming a Qobuz playlist from its manager
//! DELETES its description; the local branch writes SQL NULL. Neither is
//! ported.
//!
//! # `busy` HOLDS THE MODAL OPEN (D22)
//!
//! Save and delete disarm the buttons and keep the panel up; it closes on
//! success only, and a failure stays open and toasts. `close()` is refused
//! while busy. Same shape as the folder panels.
//!
//! **No `#[qsignal]`** — outcomes are republishes and toasts
//! (`playlist_picker_bridge.rs:13`).
//!
//! `delete` is a C++ keyword, so the member is `delete_playlist` /
//! `deletePlaylist` (§3.3). The module is `qbz_playlist_edit_bridge` — a
//! bridge module is never named after a workspace crate (E0659).

use std::pin::Pin;
use std::sync::OnceLock;

use cxx_qt::CxxQtThread;
use cxx_qt::Threading as _;
use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod qbz_playlist_edit_bridge {
    extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        // The ONE editor document (§4.6): open / id / name / description /
        // descLoaded / isLocal / offlineOnly / busy. Parseable default so a
        // binding reading `doc.open` on the pre-publish frame cannot throw.
        #[qproperty(QString, edit_json)]
        /// The "Delete playlist?" confirmation summoned from context menus:
        /// `{open, id, name, isLocal}` (`playlist_edit_qt::DeleteDoc`).
        #[qproperty(QString, delete_json)]
        /// The "New playlist" document (`playlist_create_qt`): open / busy /
        /// offlineLocked / folders. Same parseable-default rule as `edit_json`.
        ///
        /// CREATE RIDES THE EDITOR'S SINGLETON rather than getting one of its
        /// own, which is where the reference splits it (`CreatePlaylistState`
        /// vs `EditPlaylistState`). One domain — a playlist's own metadata:
        /// name, description, folder, the offline-only flag — reached from the
        /// same two surfaces (the sidebar and the manager), with two documents
        /// that never interact. A second QObject would buy the separation at
        /// the cost of a second boot() to forget, and this port has already
        /// paid that bill once: the bridge header's own note about a missing
        /// `boot()` being "silently and forever" dropped is not hypothetical.
        #[qproperty(QString, create_json)]
        type QbzPlaylistEdit = super::QbzPlaylistEditRust;

        /// Registers this object's Qt-thread hop. Without it every `ui()`
        /// publish from Rust is dropped on the floor, silently and forever:
        /// the modal mounts, no document ever arrives, `editJson` stays at
        /// its default and NOTHING is logged on either side.
        #[qinvokable]
        fn boot(self: Pin<&mut QbzPlaylistEdit>);

        /// Open the editor on `id` — either a Qobuz `u64` spelled as a string
        /// or a `local:<uuid>`. The seeds (name, description, offline-only)
        /// are resolved BEFORE the modal opens, so it never flashes an empty
        /// name field; §5.2 gives the resolution order per kind.
        ///
        /// A `local:` id resolves through a pure DB read, so the editor works
        /// offline and for an account-less user — the population local
        /// playlists exist for.
        #[qinvokable]
        fn open(self: Pin<&mut QbzPlaylistEdit>, id: QString);

        /// Persist. Name / description / offline-only are QML-LOCAL drafts and
        /// arrive as arguments (`MyQbzModals.qml`'s convention); the id and
        /// `descLoaded` are Rust-owned. `offline_only` is ignored for a Qobuz
        /// playlist — the flag is a local-playlist column.
        #[qinvokable]
        fn save(
            self: Pin<&mut QbzPlaylistEdit>,
            name: QString,
            description: QString,
            offline_only: bool,
        );

        /// Open the native image picker for the playlist held by the Rust
        /// editor state. The id is intentionally not accepted from QML: cover
        /// edits follow the same stale-document protection as Save/Delete.
        #[qinvokable]
        fn choose_cover(self: Pin<&mut QbzPlaylistEdit>);

        /// Remove the custom cover from the playlist currently being edited.
        #[qinvokable]
        fn remove_cover(self: Pin<&mut QbzPlaylistEdit>);

        /// Delete the open playlist. `delete` is a C++ keyword, hence the
        /// name. A Qobuz playlist re-derives ownership first and a FOLLOWED
        /// one is unsubscribed instead (§5.1); the back-navigation is
        /// CONDITIONAL on the user actually standing on that playlist's
        /// detail page.
        #[qinvokable]
        fn delete_playlist(self: Pin<&mut QbzPlaylistEdit>);

        /// Context menus: open the shell-level "Delete playlist?"
        /// confirmation for `id` (Qobuz id or local ref), showing `name`.
        #[qinvokable]
        fn ask_delete(self: Pin<&mut QbzPlaylistEdit>, id: QString, name: QString);
        /// The confirmation's Delete / Cancel.
        #[qinvokable]
        fn confirm_delete(self: Pin<&mut QbzPlaylistEdit>);
        #[qinvokable]
        fn cancel_delete(self: Pin<&mut QbzPlaylistEdit>);

        /// Dismiss. Refused while a save or delete is in flight (D22).
        #[qinvokable]
        fn close(self: Pin<&mut QbzPlaylistEdit>);

        // --- "New playlist" (playlist_create_qt) -------------------------

        /// Sidebar "+" — seed and open the create modal. Resolves the offline
        /// lock and the folder dropdown BEFORE it opens, so the panel never
        /// flashes an empty picker.
        #[qinvokable]
        fn open_create(self: Pin<&mut QbzPlaylistEdit>);

        /// Create. Every field is a QML-local draft and arrives here as an
        /// argument; `folder_id` is `""` for "No folder". `offline_only` (or
        /// being offline) selects the LOCAL arm — a `local_playlists` row that
        /// never reaches Qobuz. That toggle is the only reason this modal had
        /// to come back: the POC shortcut derived it from connectivity, so an
        /// online user could not create a local playlist at all.
        #[qinvokable]
        fn create_submit(
            self: Pin<&mut QbzPlaylistEdit>,
            name: QString,
            description: QString,
            folder_id: QString,
            is_public: bool,
            offline_only: bool,
        );

        /// Dismiss the create modal. Refused while the write is in flight.
        #[qinvokable]
        fn close_create(self: Pin<&mut QbzPlaylistEdit>);
    }

    impl cxx_qt::Threading for QbzPlaylistEdit {}
}

use qbz_playlist_edit_bridge::QbzPlaylistEdit;

/// Rust side of the playlist-editor bridge (plain storage, phase-1 pattern).
pub struct QbzPlaylistEditRust {
    edit_json: QString,
    create_json: QString,
    delete_json: QString,
}

impl Default for QbzPlaylistEditRust {
    fn default() -> Self {
        Self {
            // Closed and parseable: `doc.open === true` on frame 1 is false,
            // and no binding in the modal throws before the first publish.
            edit_json: QString::from("{\"open\":false}"),
            create_json: QString::from("{\"open\":false}"),
            delete_json: QString::from("{\"open\":false}"),
        }
    }
}

// ---------------------------------------------------------------------------
// The UI hop
// ---------------------------------------------------------------------------

static QT_THREAD: OnceLock<CxxQtThread<QbzPlaylistEdit>> = OnceLock::new();

/// Queue an editor mutation onto the Qt event loop (no-op before `boot()`
/// registers the thread).
pub(crate) fn ui(f: impl FnOnce(Pin<&mut QbzPlaylistEdit>) + Send + 'static) {
    if let Some(thread) = QT_THREAD.get() {
        let _ = thread.queue(f);
    }
}

impl qbz_playlist_edit_bridge::QbzPlaylistEdit {
    pub fn boot(self: Pin<&mut Self>) {
        if QT_THREAD.set(self.qt_thread()).is_err() {
            log::warn!("[qbz-qt] playlist edit Qt thread already registered");
        }
        // Publish the closed documents once, so each modal's parse sees the
        // full shape rather than the terser Default literal.
        crate::playlist_edit_qt::publish();
        crate::playlist_create_qt::publish();
        crate::playlist_edit_qt::publish_delete_ask();
    }

    pub fn open(self: Pin<&mut Self>, id: QString) {
        crate::playlist_edit_qt::open(&id.to_string());
    }

    pub fn save(self: Pin<&mut Self>, name: QString, description: QString, offline_only: bool) {
        crate::playlist_edit_qt::save(&name.to_string(), &description.to_string(), offline_only);
    }

    pub fn choose_cover(self: Pin<&mut Self>) {
        crate::playlist_edit_qt::choose_cover();
    }

    pub fn remove_cover(self: Pin<&mut Self>) {
        crate::playlist_edit_qt::remove_cover();
    }

    pub fn delete_playlist(self: Pin<&mut Self>) {
        crate::playlist_edit_qt::delete_playlist();
    }

    pub fn ask_delete(self: Pin<&mut Self>, id: QString, name: QString) {
        crate::playlist_edit_qt::ask_delete(&id.to_string(), &name.to_string());
    }

    pub fn confirm_delete(self: Pin<&mut Self>) {
        crate::playlist_edit_qt::confirm_delete();
    }

    pub fn cancel_delete(self: Pin<&mut Self>) {
        crate::playlist_edit_qt::cancel_delete();
    }

    pub fn close(self: Pin<&mut Self>) {
        crate::playlist_edit_qt::close();
    }

    pub fn open_create(self: Pin<&mut Self>) {
        crate::playlist_create_qt::open();
    }

    pub fn create_submit(
        self: Pin<&mut Self>,
        name: QString,
        description: QString,
        folder_id: QString,
        is_public: bool,
        offline_only: bool,
    ) {
        crate::playlist_create_qt::submit(
            &name.to_string(),
            &description.to_string(),
            &folder_id.to_string(),
            is_public,
            offline_only,
        );
    }

    pub fn close_create(self: Pin<&mut Self>) {
        crate::playlist_create_qt::close();
    }
}
