// Kiosk playlist: scrolling touch header and bounded native ListView delegates.
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
        // An in-place playlist switch starts at the top; the first document
        // of a mount leaves the viewport to ScrollMemory.
        if (loadedPlaylistId !== "")
            rowsModel.scrollToTop()
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
        if (root.isOwner || root.isLocal) {
            m.push({label:t("Add cover",r),icon:"image-plus",action:"cover-add"})
            if ((root.doc.coverPath || "") !== "") m.push({label:t("Remove cover",r),icon:"trash-2",action:"cover-remove"})
        }
        return m
    }

    function headerMenuAction(action) {
        if (action === "cover-add") QbzBridge.playlistCoverAdd(doc.id || "")
        else if (action === "cover-remove") QbzBridge.playlistCoverRemove(doc.id || "")
        else if (action === "play") QbzBridge.playlistPlayAll()
        else if (action === "next" || action === "later" || action === "queue")
            QbzBridge.playlistEnqueueAll(action)
        else if (action === "pin") QbzBridge.playlistTogglePin()
        else if (action === "share")
            QbzBridge.playlistShare(String(root.doc.id || ""))
        else if (action === "edit")
            QbzPlaylistEdit.open(String(root.doc.id || ""))
        else if (action === "offline")
            QbzOffline.cachePlaylist(String(root.doc.id || ""))
    }

    CardMenu { kioskHost: true;
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
            Math.round((QbzShell.dragY - tl.y + trackList.contentY) / (root.canReorder ? 88 : 64))))
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


    readonly property var sortOptions: [
        {field:"default",label:QbzSession.tr("Default",QbzSession.trRev)},
        {field:"title",label:QbzSession.tr("Title",QbzSession.trRev)},
        {field:"artist",label:QbzSession.tr("Artist",QbzSession.trRev)},
        {field:"album",label:QbzSession.tr("Album",QbzSession.trRev)},
        {field:"duration",label:QbzSession.tr("Duration",QbzSession.trRev)},
        {field:"added",label:QbzSession.tr("Date added",QbzSession.trRev)}
    ].concat(root.isLocal ? [] : [{field:"custom",label:QbzSession.tr("Custom",QbzSession.trRev)}])
    ListView {
        id: trackList
        anchors.fill: parent
        anchors.margins: 12
        clip: true
        cacheBuffer: height
        reuseItems: true
        boundsBehavior: Flickable.StopAtBounds
        // Swapped on a QbzArrayModel, never on `model:` — a fresh array there
        // makes Qt 6.11 focus delegate 0, and the header's search box lost the
        // keyboard whenever a filtered document landed (see the control).
        model: QbzArrayModel { id: rowsModel; view: trackList; rows: root.tracks; delegate: trackDelegate }
        header: Column {
            width: trackList.width
            spacing: 12
            Row {
                width: parent.width
                spacing: 16
                KioskArtwork {
                    width: 96; height: 96
                    source: root.doc.coverPath || (typeof (root.doc.covers || [])[0] === "string" ? root.doc.covers[0] : "")
                }
                Column {
                    width: parent.width - 112
                    spacing: 8
                    Text {
                        width: parent.width; text: root.doc.name || QbzSession.tr("Playlist",QbzSession.trRev)
                        color: theme.textPrimary; font.pixelSize: 22; wrapMode: Text.WordWrap
                    }
                    Text {
                        width: parent.width
                        text: (root.doc.owner || "") + " • " + root.allTracks.length + " " + QbzSession.tr("tracks",QbzSession.trRev)
                        color: theme.textSecondary; font.pixelSize: 16; elide: Text.ElideRight
                    }
                    SettingsButton {
                        visible: (root.doc.description || "") !== ""
                        kioskHost: true; text: QbzSession.tr("Read more",QbzSession.trRev)
                        onClicked: root.openDescription()
                    }
                }
            }
            Flow {
                width: parent.width
                spacing: 8
                QbzCircleAction { diameterOverride:64; primary:true; name:"play-fill"; btnEnabled:root.allTracks.length>0; onClicked:QbzBridge.playlistPlayAll() }
                QbzCircleAction { diameterOverride:64; name:"shuffle"; btnEnabled:root.allTracks.length>0; onClicked:QbzBridge.playlistShuffle() }
                QbzCircleAction { diameterOverride:64; name:root.headerFavorite ? "heart-filled":"heart"; active:root.headerFavorite; onClicked:QbzBridge.playlistToggleFavorite() }
                QbzCircleAction { diameterOverride:64; visible:!root.isOwner; name:doc.isFollowing ? "check":"user-plus"; btnEnabled:root.online; onClicked:QbzBridge.playlistToggleFollow() }
                QbzCircleAction { diameterOverride:64; visible:!root.isOwner && doc.isCopied !== true; name:"copy"; btnEnabled:root.online; onClicked:QbzBridge.playlistCopy() }
                QbzCircleAction { diameterOverride:64; name:"square-check-big"; active:root.multiSelect; onClicked:root.setMultiSelect(!root.multiSelect) }
                QbzCircleAction { id:menuButton; diameterOverride:64; name:"ellipsis"; onClicked:playlistHeaderMenu.openBelowRight(menuButton) }
            }
            Flow {
                width: parent.width; spacing:8
                QbzLineEdit { kioskHost:true; searchMode:true; width:Math.max(200,parent.width-248); text:root.searchQuery; onEdited:function(t){QbzBridge.playlistSetSearch(t)} }
                QbzSelect {
                    kioskHost:true; menuWidth:232
                    options:root.sortOptions.map(function(o){return o.label})
                    currentIndex:root.sortOptions.findIndex(function(o){return o.field===root.sortField})
                    onSelected:function(i){QbzBridge.playlistSetSort(root.sortOptions[i].field)}
                }
            }
            Flow {
                visible:root.multiSelect; width:parent.width; spacing:8
                SettingsButton { kioskHost:true; text:QbzSession.tr("Select all",QbzSession.trRev); onClicked:root.selectAll() }
                SettingsButton { kioskHost:true; text:QbzSession.tr("Clear",QbzSession.trRev); onClicked:root.bulkAction("clear") }
                SettingsButton { id:bulkButton; kioskHost:true; text:QbzSession.tr("More options",QbzSession.trRev); onClicked:bulkMenu.openBelowRight(bulkButton) }
            }
            KioskSkeleton {
                width:parent.width; height:visible ? 180 : 0
                visible:root.listPending || root.tracks.length===0 || (root.doc.error || "")!==""
                kind:"list"; loading:root.listPending; empty:root.tracks.length===0
                error:root.doc.error || ""
            }
        }
                // Handed to rowsModel: Qt 6.8/6.9 ignore a view's delegate when
                // the model is an external DelegateModel (zero rows).
                Component {
                    id: trackDelegate
                    TrackRow {
                        kioskHost: true
                        required property var modelData
                        required property int index
                        width: parent ? parent.width : 0
                        item: modelData
                        number: index + 1
                        showArtwork: false
                        showSource: root.isLocal || !root.online
                        showAlbum: false
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
                        // The DISPLAY row id, as a string. For a Qobuz row it IS
                        // the membership id (`playlist_qt.rs:363-364` sets both
                        // from `track.id`); for a LOCAL row `playlistTrackId` is a
                        // queue id — or 0 on an unresolved one — and the removal
                        // has to be keyed on the id the position map knows.
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
    }
    CardMenu {
        id:bulkMenu; kioskHost:true; menuWidth:280
        entries:[
            {label:QbzSession.tr("Play next",QbzSession.trRev),action:"play-next"},
            {label:QbzSession.tr("Play later",QbzSession.trRev),action:"play-later"},
            {label:QbzSession.tr("Add to queue",QbzSession.trRev),action:"queue"},
            {label:QbzSession.tr("Add to playlist",QbzSession.trRev),action:"add-to-playlist"},
            {label:QbzSession.tr("Add to mixtape",QbzSession.trRev),action:"add-to-mixtape"}
        ]
        onPicked:function(a){if(root.selectedCount>0)root.bulkAction(a)}
    }
    ScrollMemory { target:trackList; scope:"playlist"; relativeToOrigin:true }
    QbzScrollBar { target:trackList; anchors.right:parent.right; anchors.top:parent.top; anchors.bottom:parent.bottom }
}
