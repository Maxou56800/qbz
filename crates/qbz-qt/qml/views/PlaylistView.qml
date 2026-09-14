// Playlist detail view — the QML port of playlist/PlaylistView.slint
// (phase 17). ONE JSON document (QbzBridge.playlistJson, playlist_qt.rs
// PlaylistDoc: header + track rows + ownership/follow/pin/sort/search).
//
// Header: 150px cover (the playlist's OWN Qobuz graphic contain-fitted,
// else the member-cover mosaic via the shared cards/PlaylistCollage.qml,
// which also supplies the placeholder glyph),
// "PLAYLIST" eyebrow, name, description-short + Read
// more (the shared AppShell text modal), owner • N tracks • duration,
// the action row: Play / Shuffle / heart / follow (foreign, subscribe API) /
// copy (foreign, create+add) / multi-select / context menu. The context menu
// owns whole-playlist queueing plus pin, share, edit and offline download;
// edit opens the SHARED controls/PlaylistEditModal.qml through QbzPlaylistEdit
// (this view mounts no editor of its own; contract D20). Right edge:
// in-playlist search + the
// sort dropdown (Default/Title/Artist/Album/Duration/Date added/Custom).
//
// Track list: the exact PlaylistView row (# / 36px art / title+artist /
// Album 220px link / Duration 70 / Quality 92 / heart / cloud reserve /
// ⋯ menu with Remove-from-playlist for owners), column header, empty and
// loading states. Per-row ⋯: Play / Play next / Play later / Add to
// queue / Add to mixtape / Add to playlist / Go to artist / Go to album /
// Add|Remove from Library / Remove from playlist (owner). "Add to playlist"
// routes by the ROW's source: a Qobuz row takes the catalog arm, a local /
// Plex row of an adopted LOCAL playlist takes the local-mode one
// (QbzPlaylistPicker.openForLocalRow -> local_picker_ref_for_row).
//
// Drag & drop (issue #589): the row BODY is the drag source (press-drag
// >6px — the same gesture that drops onto sidebar playlists). A release
// INSIDE this list reorders (owner, switches the sort to Custom and
// persists the order); a release on a sidebar playlist row is the shared
// add-to-playlist drop (main.rs drag_end). The 2px accent line marks the
// insertion slot while dragging.
//
// LOADING: header + track-row QbzSkeleton placeholders in the shape of what
// is coming (the Slint mounts a bare LoadingSpinner), plus a per-item cover
// placeholder that clears when the playlist's own cover lands.
//
// Known remaining parity delta (playlist_qt.rs has the full list): Suggested
// Songs are not ported. Whole-playlist offline download is available both in
// the header context menu and on the table-header cloud glyph; both share the
// album preflight.

import QtQuick
import QtQuick.Controls
import QtQuick.Window
import com.blitzfc.qbz
import "../cards"
import "../controls"
import "../rows"
import "../theme"

Rectangle {
    id: root
    color: ambientOn ? "transparent" : theme.surfaceMain
    readonly property bool ambientOn: theme.ambientOn
    radius: 12

    QbzTheme { id: theme }

    readonly property var doc: parseDoc()
    function parseDoc() {
        try {
            return JSON.parse(QbzBridge.playlistJson)
        } catch (e) {
            return ({})
        }
    }
    readonly property var allTracks: doc.tracks || []
    readonly property bool isOwner: doc.isOwner === true
    readonly property bool loading: doc.loading === true

    // --- Multi-select (the Qobuz track rows) ------------------------------
    // Same shape as AlbumView: the selection lives in QML, select-all/clear
    // never reach Rust, everything else goes down as a JSON id array through
    // QbzPlayer.bulkTracksAction (bulk_tracks_qt.rs).
    property bool multiSelect: false
    property var selected: ({})
    readonly property int selectedCount: Object.keys(root.selected).length
    readonly property bool multiSelectOn: root.multiSelect
    property string loadedPlaylistId: ""
    function setMultiSelect(on) {
        root.multiSelect = on
        if (!on) { root.selected = ({}); sel.anchorId = "" }
    }
    /// Excel-style selection lives in ONE place — controls/SelectionModel.qml
    /// holds the anchor and the Shift-range rule; this view keeps owning its
    /// map. `mods` is the mouse event's modifiers, forwarded by the row; a
    /// caller with no event (the checkbox, a keyboard path) may omit it.
    SelectionModel { id: sel }
    function toggleSelected(id, mods) {
        root.selected = sel.next(root.selected, id, root.tracks,
                                 mods === undefined ? Qt.NoModifier : mods)
    }
    function selectedIdsInOrder() {
        var rows = root.tracks
        var out = []
        for (var i = 0; i < rows.length; i++)
            if (root.selected[rows[i].id] === true) out.push(rows[i].id)
        return out
    }
    function bulkAction(action) {
        if (action === "select-all") {
            var m = {}
            var rows = root.tracks
            for (var i = 0; i < rows.length; i++) m[rows[i].id] = true
            root.selected = m
            return
        }
        if (action === "clear") { root.selected = ({}); sel.anchorId = ""; return }
        var ids = root.selectedIdsInOrder()
        if (ids.length === 0) return
        QbzPlayer.bulkTracksAction(JSON.stringify(ids), action, "playlist", String(doc.id || ""))
        if (action !== "add-to-playlist" && action !== "add-to-mixtape")
            root.selected = ({})
    }
    // Ctrl+A / Escape hotkey router seam (AppShell duck-types these).
    function selectAll() {
        if (!root.multiSelect) root.setMultiSelect(true)
        root.bulkAction("select-all")
    }
    function exitMultiSelectMode() {
        if (root.multiSelect) root.setMultiSelect(false)
    }
    function syncPlaylistState() {
        var id = doc.id || ""
        if (id === loadedPlaylistId)
            return
        loadedPlaylistId = id
        setMultiSelect(false)
    }
    onDocChanged: syncPlaylistState()
    readonly property string sortField: doc.sortField || "default"
    readonly property bool sortAsc: doc.sortAsc === true
    readonly property string searchQuery: doc.search || ""
    /// A LOCAL detail (`local:<uuid>`) adopted into this view
    /// (local_playlist_qt.rs -> playlist_qt::adopt_doc). It drops the Qobuz-
    /// only affordances and, for the reorder below, it changes WHICH sort is
    /// the reorderable one.
    readonly property bool isLocal: doc.isLocalPlaylist === true
    // A MIXED playlist: a QOBUZ playlist carrying local-file and/or Plex rows
    // from the `library.db` sidecar tables. It is NOT `isLocal` and must not be
    // conflated with it — it keeps every Qobuz affordance (owner, follow, copy,
    // share, the heart). The one thing it loses is reorder; see `canReorder`.
    readonly property bool isMixed: doc.isMixed === true
    /// A Qobuz WRITE can be attempted. `QbzSession.offlineMode` is the shared
    /// tri-state (0 = online, 1 = no connection, 2 = induced offline — the same
    /// property `shell/HeaderBar.qml:101` and `controls/QbzOfflinePlaceholder`
    /// read), and BOTH non-zero arms refuse the request in Rust, so the buttons
    /// that depend on one must not advertise otherwise.
    readonly property bool online: QbzSession.offlineMode === 0

    // --- Reorderable? (owner findings 5 + 8) -----------------------------
    // PlaylistView.slint:1094-1096 states the rule: the reorder affordance is
    // offered under the CUSTOM sort — "or, on a LOCAL playlist, the Default
    // sort: its natural order IS the editable repo order (B2), written via
    // repo::reorder". A reorder against a COMPUTED order (title, duration, …)
    // is meaningless, which is why it is a sort test and not an ownership one
    // alone.
    //
    // The empty-search term is this port's own, and it is load-bearing: the
    // in-playlist search filters `root.tracks` HERE, in QML, while the Rust
    // arms index into the unfiltered document. Offering a reorder while a
    // filter is active would hand a filtered index to a document index — the
    // Playlist Manager takes the same precaution (`playlist_manager_qt.rs:236`
    // requires `query.is_empty()` before it lights its arrows).
    //
    // MIXED playlists are excluded outright, and this is a data-integrity gate
    // rather than a UX one: a Qobuz playlist's custom order is stored keyed by
    // `u64` catalog id, while a local row's display id is a `library.db` rowid
    // in the same numeric space, so a reorder here would write an order that
    // collides with a real track and moves the WRONG row on the next load.
    // Rust refuses it too (main.rs `playlist_reorder` / `playlist_move_row`) —
    // this only keeps the app from offering something it will not do.
    readonly property bool canReorder: root.isOwner
        && root.searchQuery.trim() === ""
        && !root.isMixed
        && (root.isLocal ? root.sortField === "default" : root.sortField === "custom")

    // --- Settled state from Rust (`libraryFavoriteChanged` / `pinChanged`) -
    // Two jobs, and the first one is why this exists at all.
    //
    // TRACK ROWS. The list is a ListView with cacheBuffer 500 — it RECYCLES.
    // A row's heart lives on the delegate (rows/TrackRow.qml), so a delegate
    // that is destroyed and rebuilt reads its state back out of `modelData`,
    // i.e. out of `allTracks`. Without patching that array, favouriting a
    // track and scrolling past it and back showed the OLD glyph again — and
    // `playlist_qt` is one of the page-lifetime documents that main.rs's
    // `emit_library_favorite` deliberately does NOT fan out to, so nothing
    // else was going to fix it either.
    //
    // The row objects are patched IN PLACE. Re-deriving `tracks` (or
    // re-parsing `doc`) would push a NEW array into `model:`, and
    // QQuickItemView::setModel() resets the scroll offset to 0 — the user
    // would be thrown to the top of the playlist on every heart click. The
    // live delegates need no array change anyway: each one settles itself
    // from this same signal.
    //
    // BOUNDARY, stated so nobody hunts it twice: this patch lasts until the
    // NEXT republish of `QbzBridge.playlistJson`, because that hands over a
    // freshly parsed object graph. `playlist_qt` keeps its document in a Rust
    // `PAGE` cache and re-serializes it as-is (covers landing, a sort change,
    // an in-playlist search), so a republish restores the heart the rows were
    // STAMPED with. Closing that needs `playlist_qt::apply_favorite_change`
    // on the Rust side — main.rs's `emit_library_favorite` doc lists it as a
    // known gap — not more QML.
    //
    // HEADER. Two one-slot overrides so the heart and the pin also answer a
    // flip made somewhere else (a sidebar card, the Home Pinned rail) instead
    // of waiting for a document republish that those paths do not trigger.
    // They carry their OWN playlist id (one slot each, never a shared one —
    // a shared id would make a pin on playlist B resurrect playlist A's stale
    // heart), so an override expires the moment the page shows anything else.
    property string favOverrideId: ""
    property bool favOverrideValue: false
    property string pinOverrideId: ""
    property bool pinOverrideValue: false
    readonly property bool headerFavorite: (root.favOverrideId !== ""
        && root.favOverrideId === (doc.id || ""))
        ? root.favOverrideValue : (doc.isFavorite === true)
    readonly property bool headerPinned: (root.pinOverrideId !== ""
        && root.pinOverrideId === (doc.id || ""))
        ? root.pinOverrideValue : (doc.pinned === true)
    Connections {
        target: QbzLibrary
        function onLibraryFavoriteChanged(key, value) {
            if (key.indexOf("track:") === 0) {
                var tid = key.substring(6)
                var t = root.allTracks
                for (var i = 0; i < t.length; i++) {
                    if (String(t[i].id) === tid) { t[i].isFavorite = value; break }
                }
                return
            }
            var pid = root.doc.id || ""
            if (pid !== "" && key === "playlist:" + pid) {
                root.favOverrideId = pid
                root.favOverrideValue = value
            }
        }
        function onPinChanged(key, value) {
            var pid = root.doc.id || ""
            if (pid !== "" && key === "playlist:" + pid) {
                root.pinOverrideId = pid
                root.pinOverrideValue = value
            }
        }
    }

    // Visible rows after the in-playlist search filter.
    readonly property var tracks: {
        if (searchQuery.trim() === "") return allTracks
        const needle = searchQuery.trim().toLowerCase()
        return allTracks.filter(function (t) {
            return (t.title || "").toLowerCase().indexOf(needle) >= 0
                || (t.artist || "").toLowerCase().indexOf(needle) >= 0
                || (t.album || "").toLowerCase().indexOf(needle) >= 0
        })
    }

    // --- skeleton pulse (QbzSkeleton's preferred drive mode) -------------
    // ONE 900ms Timer drives EVERY placeholder in this view. GATING RULE:
    // freeze on NOT VISIBLE — the view hidden, or the window minimized /
    // hidden. NEVER on lost focus (a tiling desktop keeps windows visible
    // and unfocused). Same rule as HomeView / LibraryView / AmbientField.
    property bool skelPhase: false
    readonly property bool windowShowing: root.Window.window
        ? (root.Window.window.visibility !== Window.Minimized
           && root.Window.window.visibility !== Window.Hidden)
        : true
    // True when this playlist carries artwork of its OWN (`image_rectangle`
    // — editorial playlists). playlist_qt.rs publishes coverUrl/coverPath for
    // that graphic ONLY; everything else falls back to the member-cover
    // mosaic in `doc.covers`.
    readonly property bool hasOwnArt: (doc.coverUrl || "") !== ""
    // The cover is its OWN progressive item: it clears the moment the
    // playlist's own cover lands, independently of the track rows.
    readonly property bool coverPending: root.hasOwnArt
        && (doc.coverPath || "") === ""
    // Keep the popup model declarative. The old builder lived on the header
    // Row but the popup called it through `root`, so the right-click reached
    // an undefined JS method and opened a correctly positioned EMPTY panel.
    readonly property var playlistCoverMenuModel: doc.hasCustomCover === true
        ? [
            { "label": QbzSession.tr("Change cover", QbzSession.trRev),
              "icon": "image-plus", "action": "add" },
            { "label": QbzSession.tr("Remove cover", QbzSession.trRev),
              "icon": "trash-2", "action": "remove" }
          ]
        : [
            { "label": QbzSession.tr("Add cover", QbzSession.trRev),
              "icon": "image-plus", "action": "add" }
          ]
    // The header has no data at all until the first document lands
    // (playlist_qt.rs publishes `{ loading: true }` with empty fields).
    readonly property bool headerPending: root.loading && (doc.name || "") === ""
    readonly property bool listPending: root.loading && root.allTracks.length === 0
    Timer {
        interval: 900
        repeat: true
        running: (root.headerPending || root.listPending || root.coverPending)
            && root.visible && root.windowShowing
        onTriggered: root.skelPhase = !root.skelPhase
    }

    function sortLabel() {
        return sortField === "title" ? QbzSession.tr("Title", QbzSession.trRev)
            : sortField === "artist" ? QbzSession.tr("Artist", QbzSession.trRev)
            : sortField === "album" ? QbzSession.tr("Album", QbzSession.trRev)
            : sortField === "duration" ? QbzSession.tr("Duration", QbzSession.trRev)
            : sortField === "added" ? QbzSession.tr("Date added", QbzSession.trRev)
            : sortField === "custom" ? QbzSession.tr("Custom", QbzSession.trRev)
            : QbzSession.tr("Default", QbzSession.trRev)
    }

    function headerMenuModel() {
        var t = QbzSession.tr
        var r = QbzSession.trRev
        var playable = root.allTracks.length > 0
        var m = [
            { "label": t("Play", r), "icon": "play-fill", "action": "play",
              "enabled": playable },
            { "label": t("Play next", r), "icon": "list-start", "action": "next",
              "enabled": playable },
            { "label": t("Play later", r), "icon": "list-plus", "action": "later",
              "enabled": playable },
            { "label": t("Add to queue", r), "icon": "list-end", "action": "queue",
              "enabled": playable },
            { "label": t("Add to mixtape", r), "icon": "cassette-tape", "action": "mixtape" },
            { "sep": true },
            { "label": root.headerPinned ? t("Unpin", r) : t("Pin", r),
              "icon": root.headerPinned ? "pin-filled" : "pin", "action": "pin" },
        ]
        if (!root.isLocal)
            m.push({ "label": t("Share", r), "icon": "link", "action": "share" })
        if (root.isOwner)
            m.push({ "label": t("Edit playlist", r), "icon": "pen-line", "action": "edit" })
        if (!root.isLocal) {
            var preflighting = QbzOffline.collectionPreflightLoading
                && QbzOffline.collectionPreflightKey === "playlist:" + String(root.doc.id || "")
            m.push({ "sep": true })
            m.push({ "label": t("Make available offline", r), "icon": "cloud-download",
                     "action": "offline",
                     "enabled": root.online && playable && !preflighting })
        }
        return m
    }

    function headerMenuAction(action) {
        if (action === "play") QbzBridge.playlistPlayAll()
        else if (action === "next" || action === "later" || action === "queue")
            QbzBridge.playlistEnqueueAll(action)
        else if (action === "pin") QbzBridge.playlistTogglePin()
        else if (action === "share")
            QbzBridge.playlistShare(String(root.doc.id || ""))
        else if (action === "edit")
            QbzPlaylistEdit.open(String(root.doc.id || ""))
        else if (action === "offline")
            QbzOffline.cachePlaylist(String(root.doc.id || ""))
        else if (action === "mixtape")
            // Same payload shape the cards use (QbzMyQbzAdd.open). Source-aware:
            // a local playlist carries "local", a Qobuz one "qobuz".
            QbzMyQbzAdd.open(JSON.stringify([{
                "itemType": "playlist",
                "source": root.isLocal ? "local" : "qobuz",
                "sourceItemId": String(root.doc.id || ""),
                "title": root.doc.name || ""
            }]))
    }

    CardMenu {
        id: playlistHeaderMenu
        menuWidth: 224
        entries: root.headerMenuModel()
        onPicked: function (action) { root.headerMenuAction(action) }
    }

    // --- Reorder state (the view-level half of the shared drag) ----------
    property int reorderFrom: -1
    property int reorderOver: -1
    property string reorderDropPlaylist: ""

    // The guarded mirror (PlaylistView.slint: Rust clears over-playlist-id
    // in the same turn it clears active, so the value must be captured
    // WHILE the drag is live).
    Connections {
        target: QbzShell
        function onDragOverPlaylistIdChanged() {
            if (QbzShell.dragActive) root.reorderDropPlaylist = QbzShell.dragOverPlaylistId
        }
        function onDragXChanged() { root.updateSlot() }
        function onDragYChanged() { root.updateSlot() }
        function onDragActiveChanged() {
            if (QbzShell.dragActive) return
            // Drag ended (main.rs drag_end already ran — it handled a
            // sidebar-playlist drop, if any). A release INSIDE the list and
            // NOT on a sidebar playlist reorders.
            if (root.reorderFrom >= 0
                && root.reorderDropPlaylist === ""
                && root.pointerInList()
                && root.slotFromPointer() !== root.reorderFrom
                && root.slotFromPointer() !== root.reorderFrom + 1) {
                QbzBridge.playlistReorder(root.reorderFrom, root.slotFromPointer())
            }
            root.reorderFrom = -1
            root.reorderOver = -1
            root.reorderDropPlaylist = ""
        }
    }
    function pointerInList() {
        if (!trackList) return false
        const tl = trackList.mapToItem(null, 0, 0)
        const br = trackList.mapToItem(null, trackList.width, trackList.height)
        return QbzShell.dragX >= tl.x && QbzShell.dragX <= br.x
            && QbzShell.dragY >= tl.y && QbzShell.dragY <= br.y
    }
    function slotFromPointer() {
        const tl = trackList.mapToItem(null, 0, 0)
        return Math.max(0, Math.min(tracks.length,
            Math.round((QbzShell.dragY - tl.y + trackList.contentY) / 50)))
    }
    function updateSlot() {
        if (root.reorderFrom >= 0 && QbzShell.dragActive) {
            root.reorderOver = root.pointerInList() ? root.slotFromPointer() : -1
        }
    }

    // Read more → the shared AppShell text modal (phase 16 pattern).
    function openDescription() {
        var shell = root.parent
        while (shell && shell.openTextModal === undefined) shell = shell.parent
        if (shell) shell.openTextModal(doc.name || "", doc.description || "")
    }

    // --- Circular header action (CircleAction, on-surface variant) -------


    // ============================ the view ================================
    Column {
        anchors.fill: parent
        anchors.leftMargin: 32
        anchors.rightMargin: 16
        anchors.topMargin: 11
        anchors.bottomMargin: 16
        spacing: 0

        Item { width: 1; height: 22 }

        // Header placeholder — the SHAPE of the header that is coming
        // (150px cover + eyebrow/title/description/meta bars + the round
        // action buttons), not a spinner. Replaced wholesale by the real
        // header the moment the document lands.
        QbzSkeleton {
            visible: root.headerPending
            variant: "header"
            width: parent.width
            height: 150
            coverSize: 150
            coverGap: 24
            actionCount: 6
            phase: root.skelPhase
        }

        // --- Header ---------------------------------------------------------
        Row {
            visible: !root.headerPending
            width: parent.width
            spacing: 24

            // Cover — the SAME two arms as PlaylistCard.qml (the card fix of
            // this session, applied to the detail header):
            //   1. the playlist's OWN Qobuz graphic (`image_rectangle`,
            //      published as coverUrl/coverPath) — CONTAIN, because those
            //      are landscape and cropping cuts the wordmark;
            //   2. otherwise the member-cover MOSAIC through the shared
            //      cards/PlaylistCollage.qml (no second collage), which also
            //      owns the list-music empty state.
            // `doc.covers` are MEMBER-ALBUM covers, never the playlist's own
            // artwork — binding one of them as the header image is the bug
            // this replaces.
            Rectangle {
                width: 150
                height: 150
                radius: theme.radiusMd
                color: theme.surfaceElevated
                clip: true
                // Arm 2 first: the collage is the BACKDROP. Declaration order
                // is z-order, so the resolved single cover and the skeleton
                // below must come after it.
                PlaylistCollage {
                    anchors.fill: parent
                    visible: (doc.coverPath || "") === ""
                    // A playlist that HAS its own graphic passes no urls, so
                    // it shows the placeholder while that graphic downloads
                    // instead of flashing a member mosaic (PlaylistCard rule).
                    urls: root.hasOwnArt ? [] : (doc.covers || [])
                    radius: theme.radiusMd
                }
                // `pad`, not `contain`: the playlist's own graphic is the
                // 800x380 `image_rectangle`, so a 150px square leaves two
                // ~35px bands. Flat theme grey there made the header read as
                // a small picture floating in a box; the bands now carry a
                // gradient sampled from the image's own top and bottom edges
                // (theme/RoundedImage.qml). Slint crops this same header
                // (`playlist/PlaylistView.slint:481` is `image-fit: cover`)
                // and loses half the banner — this is one of the few places
                // the reference is not the target.
                RoundedImage {
                    visible: (doc.coverPath || "") !== ""
                    anchors.fill: parent
                    source: doc.coverPath || ""
                    radius: theme.radiusMd
                    fit: root.hasOwnArt ? "pad" : "auto"
                }
                // Per-item: the cover shimmers until THIS playlist's cover
                // lands, then clears on its own. settleMs bounds it — a
                // failed download republishes the document with an empty
                // coverPath and would otherwise shimmer forever; on settle
                // the placeholder fades and the collage below takes over
                // (empty urls -> its list-music glyph, the old empty state).
                QbzSkeleton {
                    id: plCoverSkel
                    visible: root.coverPending
                    variant: "art"
                    anchors.fill: parent
                    blockRadius: theme.radiusMd
                    phase: root.skelPhase
                    settleMs: 4000
                }
                // Left-click: the lightbox (the shared CoverLightbox, same as
                // the album page). Right-click: the custom-cover menu (Qt
                // port addition — the Slint playlist header has no menu).
                MouseArea {
                    anchors.fill: parent
                    acceptedButtons: Qt.LeftButton | Qt.RightButton
                    cursorShape: Qt.PointingHandCursor
                    onClicked: function (mouse) {
                        if (mouse.button === Qt.RightButton) {
                            plCoverMenu.openAtCursor(plCoverAnchor, mouse.x, mouse.y)
                        } else {
                            var src = (doc.coverUrl || "") !== "" ? doc.coverUrl
                                : ((doc.covers || []).length > 0 ? doc.covers[0] : "")
                            if (src !== "") plCoverLightbox.openWith(src)
                        }
                    }
                }
                Item { id: plCoverAnchor; anchors.fill: parent }
            }

            // The playlist cover menu's rows (Add vs Change+Remove flips on
            // hasCustomCover). The store is Qt-first
            // (custom_playlist_covers.json — a `playlists` key inside the
            // shared custom_artwork.json would be dropped on the Slint
            // app's next write).
            QbzContextMenu {
                id: plCoverMenu
                menuWidth: 196
                Repeater {
                    id: plCoverRepeater
                    model: root.playlistCoverMenuModel
                    delegate: Rectangle {
                        required property var modelData
                        width: parent ? parent.width : 0
                        height: 33
                        radius: 5
                        color: pcmiArea.containsMouse ? theme.surfaceHover : "transparent"
                        Row {
                            anchors.fill: parent
                            anchors.leftMargin: 8
                            spacing: 8
                            QbzIcon { name: modelData.icon; width: 15; height: 15; anchors.verticalCenter: parent.verticalCenter; tintName: "secondary" }
                            Text {
                                height: parent.height
                                width: parent.width - 23
                                text: modelData.label
                                color: theme.textSecondary
                                font.pixelSize: 13
                                verticalAlignment: Text.AlignVCenter
                                elide: Text.ElideRight
                            }
                        }
                        MouseArea {
                            id: pcmiArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                plCoverMenu.close()
                                if (modelData.action === "add") QbzBridge.playlistCoverAdd(doc.id || "")
                                else if (modelData.action === "remove") QbzBridge.playlistCoverRemove(doc.id || "")
                            }
                        }
                    }
                }
            }

            CoverLightbox { id: plCoverLightbox }

            // Metadata.
            Column {
                width: parent.width - 150 - 24
                spacing: 0
                Text {
                    text: QbzSession.tr("Playlist", QbzSession.trRev).toUpperCase()
                    color: theme.textMuted
                    font.pixelSize: 11
                    font.weight: theme.weightSemibold
                    font.letterSpacing: 1.5
                }
                Item { width: 1; height: 4 }
                Text {
                    width: parent.width
                    text: doc.name || ""
                    color: theme.textPrimary
                    font.pixelSize: theme.fontSection
                    font.weight: theme.weightBold
                    wrapMode: Text.WordWrap
                }
                Item { visible: (doc.descriptionShort || "") !== ""; width: 1; height: 6 }
                Text {
                    visible: (doc.descriptionShort || "") !== ""
                    width: Math.min(parent.width, 700)
                    text: doc.descriptionShort || ""
                    color: theme.textSecondary
                    font.pixelSize: theme.fontLegal
                    wrapMode: Text.WordWrap
                }
                Text {
                    visible: (doc.description || "") !== (doc.descriptionShort || "")
                    height: 18
                    text: QbzSession.tr("Read more", QbzSession.trRev)
                    color: rmArea.containsMouse ? theme.accentHover : theme.accent
                    font.pixelSize: theme.fontLegal
                    verticalAlignment: Text.AlignVCenter
                    MouseArea {
                        id: rmArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.openDescription()
                    }
                }
                Item { width: 1; height: 8 }
                Text {
                    text: (doc.owner || "") + "  •  " + (doc.trackCount || 0) + " " + QbzSession.tr("tracks", QbzSession.trRev)
                        + "  •  " + (doc.totalDuration || "")
                    color: theme.textSecondary
                    font.pixelSize: theme.fontLegal
                }
                Item { width: 1; height: 14 }
                // Actions — buttons left, search + sort floating right.
                Row {
                    width: parent.width
                    spacing: 12
                    Row {
                        id: playlistHeaderActions
                        width: implicitWidth
                        height: 44
                        spacing: 12
                        QbzCircleAction {
                            name: "play-fill"
                            primary: true
                            btnEnabled: root.allTracks.length > 0
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzBridge.playlistPlayAll()
                        }
                        QbzCircleAction {
                            name: "shuffle"
                            btnEnabled: root.allTracks.length > 0
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzBridge.playlistShuffle()
                        }
                        QbzCircleAction {
                            name: root.headerFavorite ? "heart-filled" : "heart"
                            active: root.headerFavorite
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzBridge.playlistToggleFavorite()
                        }
                        // Follow / unfollow and Copy are Qobuz WRITES
                        // (`playlist/subscribe`, `playlist/create`): offline
                        // they stay visible but inert, matching the reference's
                        // read-only treatment.
                        QbzCircleAction {
                            visible: !root.isOwner
                            btnEnabled: root.online
                            name: (doc.isFollowing === true) ? "check" : "user-plus"
                            active: doc.isFollowing === true
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzBridge.playlistToggleFollow()
                        }
                        QbzCircleAction {
                            visible: !root.isOwner && doc.isCopied !== true
                            btnEnabled: root.online
                            name: "copy"
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzBridge.playlistCopy()
                        }
                        // Multi-select toggle (AlbumView's button, here for
                        // the playlist's track list).
                        QbzCircleAction {
                            name: "square-check-big"
                            active: root.multiSelect
                            btnEnabled: root.tracks.length > 0
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: root.setMultiSelect(!root.multiSelect)
                        }
                        QbzCircleAction {
                            id: playlistHeaderMenuButton
                            name: "ellipsis"
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: function (mouse) {
                                playlistHeaderMenu.openAtCursor(
                                    playlistHeaderMenuButton, mouse.x, mouse.y)
                            }
                        }
                    }

                    Item {
                        width: Math.max(0, parent.width
                            - playlistHeaderActions.width - 220 - 132 - 3 * 12)
                        height: 1
                    }

                    // In-playlist search.
                    QbzSearchField {
                        width: 220
                        height: 30
                        sidePadding: 9
                        glyphSize: 13
                        anchors.verticalCenter: parent.verticalCenter
                        placeholder: QbzSession.tr("Search tracks", QbzSession.trRev)
                        onEdited: function (text) { QbzBridge.playlistSetSearch(text) }
                    }
                    // Sort dropdown.
                    Rectangle {
                        width: 132
                        height: 34
                        radius: 6
                        anchors.verticalCenter: parent.verticalCenter
                        // PlaylistView.slint:855-859 — resting fill
                        // translucent under the dynamic background.
                        color: sortArea.containsMouse
                            ? theme.surfaceHover
                            : (root.ambientOn ? theme.surfaceElevatedA50 : theme.surfaceElevated)
                        border.width: 1
                        border.color: theme.borderSubtle
                        Row {
                            anchors.fill: parent
                            anchors.leftMargin: 11
                            anchors.rightMargin: 8
                            spacing: 5
                            Text {
                                width: parent.width - 13 - 5
                                anchors.verticalCenter: parent.verticalCenter
                                text: QbzSession.tr("Sort", QbzSession.trRev) + ": " + root.sortLabel()
                                color: theme.textSecondary
                                font.pixelSize: theme.fontLegal
                                elide: Text.ElideRight
                            }
                            QbzIcon { name: "chevron-down"; width: 13; height: 13; anchors.verticalCenter: parent.verticalCenter; tintName: "muted" }
                        }
                        MouseArea {
                            id: sortArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: sortMenu.openBelowRight(sortArea)
                        }
                        QbzContextMenu {
                            id: sortMenu
                            menuWidth: 172
                            Repeater {
                                model: [
                                    { "field": "default", "label": QbzSession.tr("Default", QbzSession.trRev) },
                                    { "field": "title", "label": QbzSession.tr("Title", QbzSession.trRev) },
                                    { "field": "artist", "label": QbzSession.tr("Artist", QbzSession.trRev) },
                                    { "field": "album", "label": QbzSession.tr("Album", QbzSession.trRev) },
                                    { "field": "duration", "label": QbzSession.tr("Duration", QbzSession.trRev) },
                                    { "field": "added", "label": QbzSession.tr("Date added", QbzSession.trRev) },
                                    // Manual order — enables the per-row
                                    // reorder chevrons. HIDDEN on a LOCAL
                                    // playlist (PlaylistView.slint:945-952,
                                    // B2): its Default order IS the editable
                                    // repo order, so a separate custom
                                    // sidecar is moot — and the sidecar is
                                    // u64-keyed, which a local row's id is
                                    // not.
                                    { "field": "custom", "label": QbzSession.tr("Custom", QbzSession.trRev), "ownerOnly": true, "qobuzOnly": true },
                                ]
                                delegate: Rectangle {
                                    required property var modelData
                                    visible: (modelData.ownerOnly !== true || root.isOwner)
                                        && (modelData.qobuzOnly !== true || !root.isLocal)
                                    width: parent ? parent.width : 0
                                    height: visible ? 33 : 0
                                    radius: 5
                                    color: soArea.containsMouse ? theme.surfaceHover : "transparent"
                                    Row {
                                        anchors.fill: parent
                                        anchors.leftMargin: 8
                                        spacing: 6
                                        Text {
                                            width: parent.width - 26
                                            height: parent.height
                                            text: modelData.label
                                            color: theme.textSecondary
                                            font.pixelSize: 13
                                            font.weight: root.sortField === modelData.field ? theme.weightSemibold : theme.weightRegular
                                            verticalAlignment: Text.AlignVCenter
                                        }
                                        QbzIcon {
                                            visible: root.sortField === modelData.field
                                            name: root.sortAsc ? "chevron-up" : "chevron-down"
                                            width: 12
                                            height: 12
                                            anchors.verticalCenter: parent.verticalCenter
                                            tintName: "accent"
                                        }
                                    }
                                    MouseArea {
                                        id: soArea
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: {
                                            sortMenu.close()
                                            QbzBridge.playlistSetSort(modelData.field)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Item { width: 1; height: 18 }

        // Empty. (The loading state is the row placeholders inside the
        // track-list area below — the Slint's bare LoadingSpinner said
        // "busy" but not "this is the shape of what is coming".)
        Text {
            visible: !root.loading && root.allTracks.length === 0
            text: QbzSession.tr("This playlist is empty.", QbzSession.trRev)
            color: theme.textMuted
            font.pixelSize: theme.fontBody
        }

        // --- Column header ---------------------------------------------------
        // rows/TrackListHeader.qml — the same rows/TrackCols.qml geometry the
        // TrackRows below use. Two things were wrong when this was hand-rolled
        // here, and both are now structural:
        //   * `anchors.leftMargin: 12` on a Column child is a NO-OP (a
        //     positioner owns its children's x), so the labels started 12px
        //     left of the row cells; the header component pads itself;
        //   * it was as wide as the page while the rows are the ListView's
        //     width, which is inset 14px for the scrollbar gutter — 14px of
        //     extra stretch pushed Album/Duration/Quality right. The width
        //     below is the ROW width, which is the component's contract.
        // Multi-select bulk bar — INLINE above the column header (the Local
        // Library album view's layout: bar in flow, list below).
        QbzMultiSelectBar {
            visible: root.multiSelect && root.tracks.length > 0
            width: parent.width
            selectedCount: root.selectedCount
            actions: [
                { "id": "select-all", "label": QbzSession.tr("Select all", QbzSession.trRev), "icon": "square-check-big", "danger": false, "needsSelection": false },
                { "id": "play-next", "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "danger": false, "needsSelection": true },
                { "id": "play-later", "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "danger": false, "needsSelection": true },
                { "id": "queue", "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "danger": false, "needsSelection": true },
                { "id": "add-to-playlist", "label": QbzSession.tr("Add to playlist", QbzSession.trRev), "icon": "list-music", "danger": false, "needsSelection": true },
                { "id": "add-to-mixtape", "label": QbzSession.tr("Add to Mixtape/Collection", QbzSession.trRev), "icon": "cassette-tape", "danger": false, "needsSelection": true },
                { "id": "add-to-favorites", "label": QbzSession.tr("Add to Library", QbzSession.trRev), "icon": "heart", "danger": false, "needsSelection": true },
                { "id": "make-offline", "label": QbzSession.tr("Make available offline", QbzSession.trRev), "icon": "cloud-download", "danger": false, "needsSelection": true },
                { "id": "clear", "label": QbzSession.tr("Clear", QbzSession.trRev), "icon": "x", "danger": false, "needsSelection": true }
            ]
            onAction: function (id) { root.bulkAction(id) }
        }

        TrackListHeader {
            visible: root.tracks.length > 0
            width: parent.width - 14
            showArtwork: true
            showSource: root.isLocal || !root.online
            showAlbum: true
            showDownload: true
            downloadGlyph: !root.isLocal
            downloadActionEnabled: !root.isLocal && root.online
                && root.allTracks.length > 0
            downloadActionLoading: QbzOffline.collectionPreflightLoading
                && QbzOffline.collectionPreflightKey === "playlist:" + String(root.doc.id || "")
            onDownloadClicked: QbzOffline.cachePlaylist(String(root.doc.id || ""))
            // MIRROR the delegate's arm, or the labels slide by 22 + 14 the
            // moment the list becomes reorderable (TrackListHeader's rule 2).
            showReorder: root.canReorder
        }
        Rectangle { visible: root.tracks.length > 0; width: 1; height: 3; color: "transparent" }
        Rectangle { visible: root.tracks.length > 0; width: parent.width; height: 1; color: theme.borderSubtle }
        Item { width: 1; height: 6 }

        // --- Track list -------------------------------------------------------
        Item {
            width: parent.width
            height: parent.height - 28 - 150 - 18 - 50 - (root.multiSelect ? 46 : 0)

            // Track-list placeholder: the exact TrackRow footprint (50px
            // rows, no gap, 36px art cell), one instance for the whole
            // viewport — ONE animator, not one per row (see QbzSkeleton's
            // COST note). It unmounts the moment the first rows land.
            QbzSkeleton {
                visible: root.listPending
                variant: "rowList"
                anchors.fill: parent
                anchors.rightMargin: 14
                rowH: 50
                rowGap: 0
                rowArtSize: 36
                phase: root.skelPhase
            }

            ListView {
                id: trackList
                anchors.fill: parent
                anchors.rightMargin: 14
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                cacheBuffer: 500
                model: root.tracks
                delegate: TrackRow {
                    required property var modelData
                    required property int index
                    width: parent ? parent.width : 0
                    item: modelData
                    number: index + 1
                    showArtwork: true
                    showSource: root.isLocal || !root.online
                    showAlbum: true
                    artistLink: true
                    showDownload: true
                    // Alternating row tint, like the local album page
                    // (LocalAlbumView.qml). TrackRow already owns the stripe —
                    // it paints `#07ffffff` on even `number`s — so this is the
                    // whole change.
                    zebra: true
                    selectMode: root.multiSelect
                    checked: root.selected[item.id] === true
                    onToggleSelect: function (mods) { root.toggleSelected(item.id, mods) }
                    menuShowRemove: root.isOwner
                    // Reorder chevrons (owner findings 5 + 8). `index` is the
                    // index into `root.tracks`, which under `canReorder` is
                    // the unfiltered document — the empty-search term of that
                    // predicate is what guarantees it.
                    showReorder: root.canReorder
                    canMoveUp: index > 0
                    canMoveDown: index < root.tracks.length - 1
                    onMoveUpRequested: QbzBridge.playlistMoveRow(String(item.id), -1)
                    onMoveDownRequested: QbzBridge.playlistMoveRow(String(item.id), 1)
                    onPlayRequested: QbzBridge.playlistPlayTrack(item.id)
                    onEnqueueRequested: function (m) { QbzBridge.playlistEnqueueTrack(item.id, m) }
                    // The DISPLAY row id, as a string — for a Qobuz row the
                    // CATALOG id, for a LOCAL row its library rowid / plex key.
                    // Each Rust arm resolves it to the target it needs: the
                    // Qobuz arm (`playlist_qt::remove_track`) looks up the row's
                    // MEMBERSHIP id from it (the two differ since the
                    // membership-id split), the local/sidecar arms key their
                    // repo removal on it directly. Do NOT send `playlistTrackId`
                    // here: on a LOCAL row it is a queue id (or 0 unresolved),
                    // which the position map does not know.
                    onRemoveRequested: QbzBridge.playlistRemoveTrack(String(item.id))
                    // "Find available version" (contract §6.1) — the ONE
                    // surface that offers it, and the gate is the reference's:
                    // a QOBUZ playlist the signed-in user OWNS. Qobuz refuses
                    // a write to anyone else's playlist, so offering it there
                    // would be a dead action; a LOCAL playlist's rows are not
                    // catalog memberships at all, and a mixed local/plex row in
                    // this same view carries its own `source` word, which is
                    // why the third term reads the row and not just the doc.
                    routeReplaceExternally: root.isOwner && !root.isLocal
                        && (modelData.source || "") === ""
                    // Everything Rust needs, in ONE JSON object (the
                    // `QbzMyQbzAdd.open(JSON.stringify(...))` idiom). No row
                    // INDEX is passed on purpose: `index` here is the index
                    // into the DISPLAYED list, which under a search filter or a
                    // non-default sort is not the playlist's own order — the
                    // reposition step derives the real slot from the
                    // authoritative playlist it re-fetches anyway.
                    onReplaceRequested: QbzTrackReplace.open(JSON.stringify({
                        "playlistId": String(root.doc.id || ""),
                        "playlistTrackId": String(item.playlistTrackId || item.id),
                        "trackId": String(item.id),
                        "title": item.title || "",
                        "artist": item.artist || "",
                        "album": item.album || "",
                        "isrc": item.isrc || "",
                        "durationSecs": item.durationSecs || 0
                    }))
                    // "Add to playlist" — this view serves BOTH a Qobuz
                    // playlist and a LOCAL one (`local_playlist_qt::load`
                    // publishes into this same document through
                    // `playlist_qt::adopt_doc`), so the row's id space is not
                    // uniform here and the routing reads the row's own
                    // `source` word ("" = Qobuz, "local" | "plex" = a
                    // library.db / Plex ref). The local arm hands the DISPLAY
                    // id to Rust, which resolves it through
                    // `local_playlist_qt::local_picker_ref_for_row` — a Plex
                    // row's rating key only exists in the open detail's queue
                    // snapshot and QML cannot build the ref itself.
                    routePlaylistAddExternally: modelData.source === "local"
                        || modelData.source === "plex"
                    onPlaylistAddRequested: QbzPlaylistPicker.openForLocalRow(item.id)
                    // MyQBZ "Add to mixtape" — the HOST builds the AddItem
                    // array (TrackRow does not know itemType/source).
                    //
                    // SOURCE comes off the ROW, never a literal: a Qobuz
                    // playlist's rows carry no `source` and are catalog rows,
                    // while a LOCAL playlist's rows carry "local" | "plex"
                    // (playlist_qt.rs PlaylistTrackRow.source). "plex" folds
                    // into "local" because `AddItem.source` is
                    // "qobuz" | "local" and `source_from_str`
                    // (myqbz_add_qt.rs:85-90) maps anything that is not
                    // "local" back to Qobuz — passing "plex" verbatim would
                    // store a Plex row under a catalog id again.
                    onMixtapeRequested: QbzMyQbzAdd.open(JSON.stringify([{
                        "itemType": "track",
                        "source": (item.source === "local" || item.source === "plex")
                            ? "local" : "qobuz",
                        "sourceItemId": item.id, "title": item.title || "",
                        "subtitle": item.artist || "", "artworkUrl": item.artUrl || "",
                        "year": null, "trackCount": null
                    }]))
                    onBodyDragStarted: function (n) {
                        // #589: report the source index BEFORE the shared drag.
                        // Gated on `canReorder`, not on ownership alone: under
                        // a computed sort (or an active search) this drag is
                        // ONLY the add-to-a-sidebar-playlist gesture, and
                        // claiming a source index would let a release inside
                        // the list commit a move against an order that is not
                        // the stored one.
                        if (root.canReorder) {
                            root.reorderFrom = index
                            root.reorderOver = -1
                            root.reorderDropPlaylist = ""
                        }
                    }
                }
            }

            // Drop indicator: a 2px accent line at the insertion slot,
            // hidden on the no-op slots and while outside the list.
            Rectangle {
                visible: root.reorderFrom >= 0
                    && root.reorderOver >= 0
                    && root.reorderOver !== root.reorderFrom
                    && root.reorderOver !== root.reorderFrom + 1
                x: 0
                y: Math.max(0, Math.min(parent.height - 2,
                    root.reorderOver * 50 - trackList.contentY - 1))
                width: parent.width - 14
                height: 2
                radius: 1
                color: theme.accent
            }

            // Back/forward scroll memory (controls/ScrollMemory.qml): reports
            // this container's offset while it is the live page, and restores it
            // when a back/forward step arms this route.
            ScrollMemory { target: trackList; scope: "playlist" }
            QbzScrollBar {
                target: trackList
                anchors.right: parent.right
                anchors.rightMargin: 4
                anchors.top: parent.top
                anchors.bottom: parent.bottom
            }
        }
    }
}
