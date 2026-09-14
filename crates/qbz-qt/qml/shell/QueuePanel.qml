// Right-side Queue panel — QML port of
// crates/qbz-ui/ui/shell/QueueSidebar.slint.
//
// Tabs (Queue / History), NOW PLAYING card (live heart), UP NEXT with the
// #442 "Next in queue" / "Next up" section markers and the "stop after this
// song" marker, 40-row pagination, live search filter, row actions (play /
// remove / remove-all-after / stop-after / add-to-playlist / track info /
// heart), drag reorder (6px press-drag, drop indicator + floating ghost),
// footer (count line + Clear / save-as-playlist / infinite play / sleep
// timer + search field), History tab (thumbnail rows, click replays as a
// fresh single-track queue), exact empty and no-results states.
// Data: QbzQueue.queueJson (queue_qt.rs QueueDoc) + the two scalar sleep
// properties on the same singleton.
//
// EPHEMERAL rows drop every PERSISTENCE affordance (heart, Add to playlist,
// Track info) exactly as QueueSidebar.slint gates on `item.is-ephemeral`:
// an ephemeral play must leave nothing behind inside QBZ.

import QtQuick
import QtQuick.Controls
import QtQuick.Window
import com.blitzfc.qbz
import "../controls"
import "../theme"

Rectangle {
    id: root
    color: "transparent"
    // Clipped ONLY when stacked with the other panel. The drag ghost really
    // does leave this rect, but in the single-panel case the panel coincides
    // exactly with queueColumn, which already clips (AppShell.qml) — so the
    // scissor is pure duplication there, and duplication costs a batch root.
    clip: QbzShell.queueOpen && QbzShell.lyricsOpen

    // Queue (0) / History (1) — Slint QueueState.tab.
    property int tab: 0
    readonly property var doc: JSON.parse(QbzQueue.queueJson)
    readonly property var currentRow: doc.current || null
    readonly property var upcoming: doc.upcoming || []
    readonly property var historyRows: doc.history || []
    readonly property var settingsDoc: {
        try { return JSON.parse(QbzBridge.settingsJson) }
        catch (e) { return ({}) }
    }
    // Opt-in: Up Next swaps its numeral for the same thumbnail History uses.
    readonly property bool queueTrackArtwork:
        settingsDoc.queueTrackArtwork === true
    readonly property bool queueEmpty: currentRow === null && (doc.upcomingTotal || 0) === 0
    // "Stop after this song": the decimal id of the marked track, "" for none.
    readonly property string stopAfterId: doc.stopAfterId || ""
    // A search is active but nothing matched — a different state from an empty
    // queue, and the reason the reference echoes the query back.
    readonly property bool noSearchResults: !queueEmpty
        && (doc.searchQuery || "") !== "" && (doc.upcomingTotal || 0) === 0
    // The now-playing track is ephemeral -> the queue cannot be persisted, so
    // the footer's save-as-playlist disappears (QueueSidebar.slint:850).
    readonly property bool currentEphemeral: currentRow !== null && currentRow.isEphemeral === true

    // The panel is `visible`-gated rather than conditionally mounted, so this
    // is the equivalent of the reference's `init => QueueState.panel-opened()`:
    // pull a fresh snapshot each time it comes back on screen, because the
    // queue can change while it is hidden (a session restore, a play from any
    // view). Cheap — an unchanged document never reaches Qt.
    onVisibleChanged: {
        if (!visible)
            return
        QbzQueue.queuePanelOpened()
        // AND re-dispatch the covers: the publish dedup means
        // `queuePanelOpened` above often changes nothing, so it cannot be
        // relied on to trigger the doc change either. (This handler runs
        // OUTSIDE the docChanged storm, which is why it used to be the only
        // dispatch that asked for the right urls — see `dispatchCovers`.)
        dispatchCovers()
    }

    // --- Favourite state, id-keyed (the ArtistView.localToggles idiom) ---
    // The rows here are plain JS objects parsed out of `doc`, so the old
    // `row.isFavorite = !row.isFavorite` notified nothing and NO glyph in
    // this panel ever moved on click — not the Up Next menu entry, not the
    // Now Playing heart. A map at root level is what fits: the queue
    // document is re-parsed constantly (every position tick, every queue
    // edit), so a per-delegate property alone would be overwritten by the
    // next republish carrying the pre-toggle value.
    //
    // Both writers agree on the same map: the optimistic flip, and
    // `libraryFavoriteChanged` — which carries what the write ACTUALLY
    // produced, so it is also the rollback for a failed one and the way a
    // heart set on another surface reaches this panel.
    property var favOverrides: ({})
    function favState(row) {
        if (!row) return false
        return root.favOverrides[row.id] !== undefined
            ? root.favOverrides[row.id]
            : row.isFavorite === true
    }
    function setFavState(id, value) {
        var m = root.favOverrides
        m[id] = value
        // Rebind requires a NEW object reference (same-ref is not a change).
        root.favOverrides = Object.assign({}, m)
    }
    function toggleFav(row) {
        if (!row)
            return
        root.setFavState(row.id, !root.favState(row))
        QbzQueue.queueToggleFavorite("track", row.id)
    }
    Connections {
        target: QbzLibrary
        function onLibraryFavoriteChanged(key, value) {
            if (key.indexOf("track:") === 0)
                root.setFavState(key.substring(6), value)
        }
    }

    // url-keyed cover map (shared artwork pipeline).
    property var coverMap: ({})

    // --- UP NEXT drag-reorder state (QueueSidebar.slint:371-410) ---------
    // Panel-local on purpose: this is NOT the QbzShell drag global (that one
    // is the track -> sidebar-playlist ghost, and its drag_end() posts an
    // "add tracks to playlist" — a reorder must never reach it).
    // ALL indices here are PAGE-LOCAL, which is exactly what queue_qt.rs's
    // move_track takes (it resolves page-local -> queue-wide itself, filter
    // and pagination included) — same contract as play/remove/remove-after.
    property bool reorderActive: false
    property int reorderFrom: -1      // page-local index of the dragged row
    property int reorderOver: -1      // insertion slot, 0..N
    property real reorderPointerY: 0  // live pointer WINDOW y (drives the ghost)

    QbzTheme { id: theme }

    // Pointer window-Y -> insertion slot [0..N]. The Slint does this with a
    // fixed 44px stride plus a 22px correction per #442 section header; here
    // the rows' real geometry answers it directly (the delegate exposes its
    // row center in window space), so section headers, the list padding and
    // the 8px inter-row spacing are all accounted for without a second
    // arithmetic model to keep in sync. Top half of row k -> slot k, bottom
    // half -> slot k+1, past the last row -> N (append). <=40 rows per page.
    function slotFromPointer(winY) {
        var n = root.upcoming.length
        for (var i = 0; i < n; i++) {
            var d = upcomingRepeater.itemAt(i)
            if (d && winY < d.rowCenterY())
                return i
        }
        return n
    }

    // --- EXTERNAL drop: a track dragged in from a list (owner ask
    // 2026-08-10, "draggear hacia el queue definiendo la posición") ---------
    //
    // Distinct from the panel-local reorder above, and deliberately so: that
    // one moves a row that is ALREADY in the queue and commits through
    // `queueMoveTrack`; this one inserts a row that is not, and commits
    // through the SHARED drag (`QbzShell.dragEnd` -> `insert_dragged_track`).
    // They share only `slotFromPointer`, which is the part that is genuinely
    // the same question.
    //
    // Same self-detection shape as the sidebar's playlist rows: the target
    // reads the shared window-coord pointer and claims or releases itself,
    // because the mouse grab belongs to the row being dragged and no hover
    // event ever reaches us.
    property bool queueDropHot: false
    function recomputeQueueDrop() {
        // Never claim while the panel's OWN reorder is running, or a row being
        // reordered inside the queue would also read as an insert.
        if (!QbzShell.dragActive || root.reorderActive || !root.visible) {
            if (root.queueDropHot) { QbzShell.dragSetOverQueue(-1); root.queueDropHot = false }
            return
        }
        // The hot region is the UP NEXT LIST, not the whole panel. It used to
        // be `root`, which meant the header, the now-playing card and the
        // footer buttons all silently accepted a drop — landing it at slot 0
        // or at the end depending on which side of the rows you happened to
        // release over. A drop you did not aim is worse than a drop that does
        // not take.
        // EMPTY QUEUE is the one case where the whole panel is the target:
        // the UP NEXT section is not mounted (`upcomingTotal > 0`), so there
        // are no rows to aim at and the only possible slot is 0. Without this
        // arm, narrowing the region to the list would have made a drop into
        // an empty queue impossible — the very case the prompt exists for.
        const host = upNextSection.visible ? upNextSection : root
        const tl = host.mapToItem(null, 0, 0)
        const inside = QbzShell.dragX >= tl.x && QbzShell.dragX <= tl.x + host.width
            && QbzShell.dragY >= tl.y && QbzShell.dragY <= tl.y + host.height
        if (!inside) {
            if (root.queueDropHot) { QbzShell.dragSetOverQueue(-1); root.queueDropHot = false }
            return
        }
        // Re-publish on every move while inside: the slot changes as the
        // pointer travels, and the insertion line reads it.
        QbzShell.dragSetOverQueue(root.slotFromPointer(QbzShell.dragY))
        root.queueDropHot = true
    }
    Connections {
        target: QbzShell
        function onDragXChanged() { root.recomputeQueueDrop() }
        function onDragYChanged() { root.recomputeQueueDrop() }
        function onDragActiveChanged() { root.recomputeQueueDrop() }
    }

    // Commit — QueueSidebar.slint commit-reorder(). `to == from` and
    // `to == from + 1` both drop back onto the SAME gap, so neither reaches
    // the bridge (move_track's from == to early return would swallow them
    // anyway, at the cost of a queue round-trip and a republish).
    function commitReorder() {
        var from = root.reorderFrom
        var to = root.reorderOver
        root.reorderActive = false
        root.reorderFrom = -1
        root.reorderOver = -1
        if (from < 0 || to < 0 || to === from || to === from + 1)
            return
        QbzQueue.queueMoveTrack(from, to)
    }

    Connections {
        target: QbzLibrary
        function onLibraryArtworkReady(key, path) {
            var m = root.coverMap
            m[key] = path
            root.coverMap = Object.assign({}, m)
        }
    }
    Component.onCompleted: dispatchCovers()
    onDocChanged: dispatchCovers()
    onQueueTrackArtworkChanged: dispatchCovers()
    // Collect the covers of the document CURRENTLY on the property and ask the
    // shared pipeline for them.
    //
    // It re-derives current/upcoming/history from `root.doc` instead of reading
    // the `currentRow` / `upcoming` / `historyRows` bindings, and that is the
    // whole fix: called from `onDocChanged`, those three derived properties are
    // still holding the PREVIOUS document. Measured on this Qt build (three
    // publishes, one per event-loop turn): reading the derived properties in
    // the handler dispatched [], then A's urls on B's publish, then B's on C's
    // — always exactly one publish behind, so the row on screen NEVER had its
    // cover requested. Re-reading `root.doc` in the same handler dispatched
    // A, B, C in step.
    //
    // It is also why the defect looked source-dependent: an ALBUM queue has one
    // cover for every row, so asking one publish late asks for the same url and
    // is indistinguishable from correct. A PLAYLIST queue (a different album per
    // row) and a mixed LOCAL queue ask for the previous row's cover forever.
    function dispatchCovers() {
        var d = root.doc
        var cur = d.current || null
        var up = d.upcoming || []
        var hist = d.history || []
        var urls = []
        if (cur && cur.artUrl) urls.push(cur.artUrl)
        var i
        if (root.queueTrackArtwork)
            for (i = 0; i < up.length; i++) if (up[i].artUrl) urls.push(up[i].artUrl)
        for (i = 0; i < hist.length; i++) if (hist[i].artUrl) urls.push(hist[i].artUrl)
        if (urls.length > 0) {
            QbzShell.sidebarArtworkWindow(JSON.stringify(urls))
            root.artPulse = true
            artPulseOff.restart()
        }
    }

    // --- skeleton pulse (PER-ITEM COVERS ONLY) ---------------------------
    // The queue is LOCAL state — it never "loads", so this panel has no
    // list-level skeleton ON PURPOSE: there is no asynchronous fetch to
    // stand in for. What IS asynchronous is each row's cover, which is why
    // the placeholders here are per-item and nothing else.
    // GATING RULE: freeze on NOT VISIBLE — the panel collapsed, or the
    // window minimized/hidden. NEVER on lost focus (a tiling desktop keeps
    // windows visible and unfocused).
    property bool skelPhase: false
    readonly property bool windowShowing: root.Window.window
        ? (root.Window.window.visibility !== Window.Minimized
           && root.Window.window.visibility !== Window.Hidden)
        : true
    // A cover that never resolves (no art, or a failed fetch) leaves the map
    // untouched, so every placeholder is bounded — see QbzSkeleton settleMs.
    readonly property int artSettleMs: 2500
    property bool artPulse: false
    Timer {
        id: artPulseOff
        interval: root.artSettleMs
        onTriggered: root.artPulse = false
    }
    Timer {
        interval: 900
        repeat: true
        running: root.artPulse && root.visible && root.windowShowing
        onTriggered: root.skelPhase = !root.skelPhase
    }
    function coverPending(url) {
        return (url || "") !== "" && (root.coverMap[url] || "") === ""
    }

    component QueueTab: Text {
        property bool active: false
        signal clicked()

        font.pixelSize: theme.fontBody
        font.weight: theme.weightSemibold
        color: active ? theme.textPrimary
             : tabArea.containsMouse ? theme.textSecondary : theme.textMuted
        verticalAlignment: Text.AlignVCenter
        height: parent ? parent.height : 0

        MouseArea {
            id: tabArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: parent.clicked()
        }
    }

    // Row action icon button (the panel's small IconButton).


    // One UP NEXT / History row (QueueRow.slint).
    component QueueRow: Rectangle {
        id: qrRoot
        property var row: ({})
        property int rowIndex: 0
        property bool showNumber: true
        property bool inQueue: true
        readonly property bool artworkLeading:
            !qrRoot.showNumber || root.queueTrackArtwork
        /// Drag-reorder source? UP NEXT rows only — History has no order to
        /// commit, and it keeps its drag-scroll because of this (see qrArea's
        /// preventStealing).
        property bool reorderable: false

        readonly property bool hovered: qrArea.containsMouse || menuArea.containsMouse
        /// Pulled from the catalogue, and not playable from disk. D5 keeps
        /// these out of a queue BUILD, so the only way one reaches this list is
        /// a session restored from disk or a listing whose endpoint never
        /// reported availability — the two cases the reactive backstop exists
        /// for. Rare is not never, and an unmarked dead row in the queue is the
        /// same lie D4 forbids everywhere else.
        ///
        /// `cacheStatus` is read the same way rows/TrackRow.qml reads it and
        /// for the same reason (a pulled track the user DOWNLOADED still plays
        /// — contract §5.3/F5). `queue_qt::QueueRow` does not carry the field
        /// today, so it reads `undefined !== 3` = true and a downloaded-but-
        /// pulled queue row would be dimmed; the cost of that is cosmetic
        /// (nothing here gates playback on it) and it corrects itself the day
        /// the producer carries it.
        readonly property bool pulledDead: row.qobuzUnavailable === true
            && row.cacheStatus !== 3
        readonly property bool isActive: inQueue && QbzPlayer.npTrackId !== "" && QbzPlayer.npTrackId === row.id
        readonly property bool dragSource: reorderable && root.reorderActive
            && root.reorderFrom === rowIndex

        // Row center in WINDOW space — what slotFromPointer() measures the
        // pointer against. A function, not a binding: mapToItem() is not
        // reactive, and it is only ever read during a live drag.
        function rowCenterY() {
            return qrRoot.mapToItem(null, 0, qrRoot.height / 2).y
        }

        height: 44
        radius: theme.radiusSm
        color: (hovered && !qrRoot.pulledDead) ? theme.surfaceHover
             : (rowIndex % 2 === 1 ? "#592a2a2a" : "transparent")
        border.width: dragSource ? 1 : 0
        border.color: theme.accent
        // The dead-row dim rides the SAME channel as the drag-source dim so the
        // two cannot fight; a dragged dead row is dimmer still, which is
        // harmless and honest.
        opacity: dragSource ? 0.4 : (qrRoot.pulledDead ? 0.5 : 1.0)

        Row {
            anchors.fill: parent
            anchors.leftMargin: theme.spacingSm
            anchors.rightMargin: theme.spacingXs
            anchors.topMargin: 4
            anchors.bottomMargin: 4
            spacing: 9

            // Leading: track number (default UP NEXT) or thumbnail (History,
            // plus opt-in UP NEXT). On the row carrying the "stop after this
            // song" marker, the status still owns this fixed slot.
            Item {
                visible: showNumber && !root.queueTrackArtwork
                width: 22
                height: parent.height
                readonly property bool marked: qrRoot.inQueue
                    && root.stopAfterId !== "" && row.id === root.stopAfterId
                Text {
                    visible: !parent.marked && !qrRoot.pulledDead
                    anchors.centerIn: parent
                    width: parent.width
                    text: rowIndex + 1 + (doc.page || 0) * 40
                    color: theme.textMuted
                    font.pixelSize: 12
                    horizontalAlignment: Text.AlignHCenter
                }
                QbzIcon {
                    visible: parent.marked
                    anchors.centerIn: parent
                    name: "circle-stop"
                    width: 13
                    height: 13
                    tintName: "accent"
                }
                // Same glyph and the same reason as rows/TrackRow.qml: the
                // alert takes the number's slot so the list does not reflow,
                // and it is `circle-alert` rather than `triangle-alert` to stay
                // clear of the cache-failed meaning that row's download column
                // gives the triangle.
                //
                // The stop-after marker wins the slot when both apply — it is
                // a marker the USER placed on this exact row and removing it
                // silently would be worse than leaving one dead row unglyphed
                // (the dim still marks it, and D5 means the pair is close to
                // unreachable anyway).
                QbzIcon {
                    visible: qrRoot.pulledDead && !parent.marked
                    anchors.centerIn: parent
                    name: "circle-alert"
                    width: 13
                    height: 13
                    tintName: "favorite"
                }
            }
            Rectangle {
                visible: qrRoot.artworkLeading
                width: 34
                height: 34
                radius: 4
                anchors.verticalCenter: parent.verticalCenter
                color: (theme.ambientOn ? theme.surfaceElevatedA50 : theme.surfaceElevated)
                // No clip: RoundedImage self-confines. One batch root per
                // visible queue row.
                RoundedImage {
                    anchors.fill: parent
                    source: qrRoot.artworkLeading ? (root.coverMap[row.artUrl] || "") : ""
                    radius: 4
                }
                // Per-item: this thumbnail clears when ITS cover lands, so
                // History fills in progressively rather than all at once.
                QbzSkeleton {
                    variant: "art"
                    anchors.fill: parent
                    blockRadius: 4
                    visible: root.coverPending(row.artUrl)
                    phase: root.skelPhase
                    settleMs: root.artSettleMs
                }
                // When artwork replaces the numeral, preserve the two states
                // that used to occupy that slot. The scrim makes either glyph
                // legible without growing or shifting the 44px row.
                Rectangle {
                    readonly property bool marked: qrRoot.inQueue
                        && root.stopAfterId !== "" && row.id === root.stopAfterId
                    visible: qrRoot.showNumber
                        && (marked || qrRoot.pulledDead)
                    anchors.fill: parent
                    radius: 4
                    color: "#a6000000"
                    QbzIcon {
                        anchors.centerIn: parent
                        name: parent.marked ? "circle-stop" : "circle-alert"
                        width: 14
                        height: 14
                        tintName: parent.marked ? "accent" : "favorite"
                    }
                }
            }

            // Title + artist.
            Column {
                width: parent.width - (qrRoot.artworkLeading ? 34 : 22)
                    - durText.width - 32 - 3 * 9
                anchors.verticalCenter: parent.verticalCenter
                spacing: 2
                Row {
                    spacing: 5
                    Text {
                        text: row.title
                        color: theme.textPrimary
                        font.pixelSize: 12
                        font.weight: theme.weightMedium
                        elide: Text.ElideRight
                        width: Math.min(implicitWidth, parent.parent.width - (row.explicit ? 21 : 0))
                    }
                    Rectangle {
                        visible: row.explicit
                        width: 16
                        height: 16
                        radius: 3
                        anchors.verticalCenter: parent.verticalCenter
                        color: theme.surfaceElevated
                        Text {
                            anchors.centerIn: parent
                            text: "E"
                            color: theme.textMuted
                            font.pixelSize: 9
                            font.weight: theme.weightSemibold
                        }
                    }
                }
                Text {
                    width: parent.width
                    text: row.artist
                    color: theme.textMuted
                    font.pixelSize: 10
                    elide: Text.ElideRight
                }
            }
            Text {
                id: durText
                anchors.verticalCenter: parent.verticalCenter
                text: row.duration
                color: theme.textMuted
                font.pixelSize: 11
            }
            // ⋯ menu trigger (UP NEXT rows only; always visible, 32px hit).
            Rectangle {
                visible: inQueue
                width: 32
                height: 32
                radius: theme.radiusSm
                anchors.verticalCenter: parent.verticalCenter
                color: menuArea.containsMouse ? theme.surfaceElevated : "transparent"
                QbzIcon {
                    anchors.centerIn: parent
                    name: "ellipsis"
                    width: 16
                    height: 16
                    tintName: menuArea.containsMouse ? "textPrimary" : "muted"
                }
                MouseArea {
                    id: menuArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: function (mouse) { rowMenu.openAtCursor(menuArea, mouse.x, mouse.y) }
                }
                QbzContextMenu {
                    id: rowMenu
                    menuWidth: 196
                        // QueueSidebar.slint's track menu, in its order. The
                        // last entry is this port's own addition (the reference
                        // has no heart in the queue row menu) and is kept
                        // BELOW the ported set so the 1:1 block reads intact.
                        // EPHEMERAL rows drop the two persistence entries, the
                        // same gate the .slint applies (and the reason its menu
                        // is 105px tall instead of 175px on those rows).
                        Repeater {
                            model: {
                                var items = [
                                    { "label": QbzSession.tr("Remove from queue", QbzSession.trRev), "icon": "trash-2", "action": "remove" },
                                    { "label": row.id === root.stopAfterId
                                        ? QbzSession.tr("Cancel stop after this", QbzSession.trRev)
                                        : QbzSession.tr("Stop after this", QbzSession.trRev), "icon": "circle-stop", "action": "stop-after" },
                                    { "label": QbzSession.tr("Remove all after", QbzSession.trRev), "icon": "list-x", "action": "remove-after" },
                                ]
                                if (row.isEphemeral !== true) {
                                    // LOCAL/Plex rows drop it too: the queue's
                                    // `add_to_playlist` refuses them
                                    // (queue_qt.rs — a queue row's id is a
                                    // library.db / Plex synthetic id and this
                                    // panel has no source-aware resolver to
                                    // turn it into a local-mode ref), so the
                                    // entry would render and no-op. Absent
                                    // beats dead; the LOCAL surfaces that DO
                                    // have the resolver offer it instead
                                    // (local/LocalTrackRow.qml).
                                    if (row.isLocal !== true)
                                        items.push({ "label": QbzSession.tr("Add to playlist", QbzSession.trRev), "icon": "list-plus", "action": "add-to-playlist" })
                                    items.push({ "label": QbzSession.tr("Track info", QbzSession.trRev), "icon": "info", "action": "track-info" })
                                    items.push({ "label": root.favState(row) ? QbzSession.tr("Remove from Library", QbzSession.trRev) : QbzSession.tr("Add to Library", QbzSession.trRev), "icon": root.favState(row) ? "heart-filled" : "heart", "action": "favorite" })
                                }
                                return items
                            }
                            delegate: Rectangle {
                                required property var modelData
                                width: parent ? parent.width : 0
                                height: 33
                                radius: 5
                                color: rmiArea.containsMouse ? theme.surfaceHover : "transparent"
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
                                    id: rmiArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: {
                                        rowMenu.close()
                                        var a = modelData.action
                                        if (a === "remove") QbzQueue.queueRemoveUpcoming(rowIndex)
                                        else if (a === "stop-after") QbzQueue.queueToggleStopAfter(row.id)
                                        else if (a === "remove-after") QbzQueue.queueRemoveAllAfter(rowIndex)
                                        else if (a === "add-to-playlist") QbzQueue.queueAddToPlaylist(rowIndex)
                                        else if (a === "track-info") qrRoot.openTrackInfo()
                                        else if (a === "favorite") root.toggleFav(row)
                                    }
                                }
                            }
                        }
                    }
            }
        }

        // Row body: click plays, >6px press-drag reorders (QueueSidebar
        // .slint's `ta` does both from one TouchArea — one area per row, not
        // two stacked).
        MouseArea {
            id: qrArea
            anchors.fill: parent
            // BEHIND the row's own controls. Declaration order IS z-order in
            // QML and this fills the row from the last declaration, so at the
            // default z it sat on top of the ⋯ button and swallowed its press
            // — the menu never opened, the row just played. Same fix (and the
            // same reason the .slint declares its TouchArea BEFORE the
            // content) as rows/TrackRow.qml's trArea.
            z: -1
            hoverEnabled: true
            // Arrow on a dead row, for the reason rows/TrackRow.qml gives: the
            // pointer must not promise a play the click below refuses.
            cursorShape: qrRoot.pulledDead ? Qt.ArrowCursor : Qt.PointingHandCursor
            // THE reason drag-reorder scrolled instead of reordering: the
            // enclosing Flickable filters its children's mouse events and
            // grabs the press once the pointer moves past
            // QStyleHints.startDragDistance (~10px), after which this area
            // gets `canceled`, never `released`. Keeping the grab here is the
            // Qt equivalent of QueueSidebar.slint's `interactive: false` on
            // the queue Flickable — but scoped to the row and without that
            // side effect: Qt's Flickable also drops WHEEL events when it is
            // non-interactive, so wheel + scrollbar keep working, and only a
            // press that starts ON an UP NEXT row is denied to the flick.
            // (Consequence, same as the Slint: no drag-scroll while
            // reordering — pagination covers long lists.)
            preventStealing: qrRoot.reorderable
            property real downY: 0
            property bool dragging: false
            // A release that ended a drag must NOT also play the track
            // (`clicked` fires after `released`).
            property bool suppressClick: false
            onPressed: function (mouse) {
                qrArea.downY = qrArea.mapToItem(null, mouse.x, mouse.y).y
                qrArea.dragging = false
            }
            onPositionChanged: function (mouse) {
                if (!qrRoot.reorderable || !qrArea.pressed)
                    return
                // WINDOW space for both the threshold and the slot, so a
                // wheel-scroll mid-gesture cannot corrupt either.
                var winY = qrArea.mapToItem(null, mouse.x, mouse.y).y
                if (!qrArea.dragging && Math.abs(winY - qrArea.downY) > 6) {
                    qrArea.dragging = true
                    root.reorderActive = true
                    root.reorderFrom = qrRoot.rowIndex
                    root.reorderOver = qrRoot.rowIndex
                }
                if (qrArea.dragging) {
                    root.reorderPointerY = winY
                    root.reorderOver = root.slotFromPointer(winY)
                }
            }
            onReleased: {
                if (!qrArea.dragging)
                    return
                qrArea.dragging = false
                qrArea.suppressClick = true
                root.commitReorder()
            }
            // Safety net (the .slint keeps one too): if the grab is lost
            // mid-drag the release never arrives here, and without this the
            // panel would stay stuck in reorderActive with a ghost pinned to
            // the last pointer position.
            onCanceled: {
                if (!qrArea.dragging)
                    return
                qrArea.dragging = false
                qrArea.suppressClick = true
                root.commitReorder()
            }
            onClicked: {
                if (qrArea.suppressClick) {
                    qrArea.suppressClick = false
                    return
                }
                // D4 again: a dead row is inert on click. The ⋯ menu is
                // untouched, so "Remove from queue" — the one thing the user
                // actually wants to do with this row — still works, and the
                // press-drag reorder above still works too (it commits before
                // this handler ever runs).
                if (qrRoot.pulledDead)
                    return
                if (inQueue) QbzQueue.queuePlayUpcoming(rowIndex)
                else QbzQueue.queuePlayHistory(rowIndex)
            }
        }

        // Drop indicator — the .slint draws one 2px accent line at the
        // insertion slot, hidden on the two no-op slots (from, from+1). Here
        // it rides the row that OWNS the slot (its top edge, and the last
        // row's bottom edge for the append slot), which needs no overlay
        // geometry and stays correct across the section headers.
        Rectangle {
            visible: qrRoot.reorderable && root.reorderActive
                && root.reorderOver === qrRoot.rowIndex
                && root.reorderOver !== root.reorderFrom
                && root.reorderOver !== root.reorderFrom + 1
            y: -1
            width: parent.width
            height: 2
            radius: 1
            color: theme.accent
        }
        Rectangle {
            visible: qrRoot.reorderable && root.reorderActive
                && root.reorderOver === root.upcoming.length
                && qrRoot.rowIndex === root.upcoming.length - 1
                && root.reorderFrom !== root.upcoming.length - 1
            y: parent.height - 1
            width: parent.width
            height: 2
            radius: 1
            color: theme.accent
        }
        // The SAME two lines for an EXTERNAL drop (a row dragged in from a
        // track list). Driven by the shared `dragOverQueueIndex` instead of
        // the panel-local `reorderOver`, and with none of the no-op-slot
        // suppression: every slot is a real destination for a row that is not
        // in the queue yet, including the two a reorder would ignore.
        Rectangle {
            visible: root.queueDropHot && QbzShell.dragOverQueueIndex === qrRoot.rowIndex
            y: -1
            width: parent.width
            height: 2
            radius: 1
            color: theme.accent
        }
        Rectangle {
            visible: root.queueDropHot
                && QbzShell.dragOverQueueIndex === root.upcoming.length
                && qrRoot.rowIndex === root.upcoming.length - 1
            y: parent.height - 1
            width: parent.width
            height: 2
            radius: 1
            color: theme.accent
        }

        // --- Track info (rows/TrackRow.qml's lazy-Loader idiom) -------------
        // A full Popup tree per delegate would cost 40 of them on a full page,
        // so it is activated on demand and torn down on close (Qt.callLater so
        // the Popup is not destroyed inside its own signal).
        Loader {
            id: trackInfoLoader
            active: false
            sourceComponent: TrackInfoModal { }
        }
        Connections {
            target: trackInfoLoader.item
            ignoreUnknownSignals: true
            function onClosed() { Qt.callLater(function () { trackInfoLoader.active = false }) }
        }
        function openTrackInfo() {
            if (!qrRoot.row || !qrRoot.row.id)
                return
            trackInfoLoader.active = true
            trackInfoLoader.item.openFor(qrRoot.row.id)
        }
    }

    Column {
        anchors.fill: parent
        spacing: 0

        // --- Header: tabs + close button -------------------------------
        Item {
            width: parent.width
            height: 44

            Row {
                anchors.left: parent.left
                anchors.leftMargin: theme.spacingMd
                anchors.right: closeBtn.left
                anchors.rightMargin: theme.spacingSm
                anchors.verticalCenter: parent.verticalCenter
                height: 28
                spacing: 14

                QbzIconButton {
                    btnSize: 28
                    iconSize: 15
                    name: "panel-right-open"
                    tooltip: tips
                    tooltipKey: "queue-view-open"
                    tooltipText: QbzSession.tr("Listen list", QbzSession.trRev)
                    onClicked: QbzShell.navigateTo("queue-view")
                }
                QueueTab {
                    text: QbzSession.tr("Queue", QbzSession.trRev)
                    active: root.tab === 0
                    onClicked: root.tab = 0
                }
                Text {
                    text: "|"
                    color: theme.textDisabled
                    font.pixelSize: theme.fontBody
                    height: parent.height
                    verticalAlignment: Text.AlignVCenter
                }
                QueueTab {
                    text: QbzSession.tr("History", QbzSession.trRev)
                    active: root.tab === 1
                    onClicked: root.tab = 1
                }
            }
            Rectangle {
                id: closeBtn
                anchors.right: parent.right
                anchors.rightMargin: theme.spacingSm
                anchors.verticalCenter: parent.verticalCenter
                width: 28
                height: 28
                radius: theme.radiusSm
                color: closeArea.containsMouse ? theme.surfaceHover : "transparent"
                QbzIcon {
                    name: "x"
                    width: 15
                    height: 15
                    anchors.centerIn: parent
                    tintName: closeArea.containsMouse ? "textPrimary" : "secondary"
                }
                MouseArea {
                    id: closeArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: QbzShell.toggleQueue()
                }
            }
        }
        Rectangle {
            width: parent.width
            height: 1
            color: theme.borderSubtle
        }

        // --- Body --------------------------------------------------------
        Item {
            width: parent.width
            height: parent.height - 45 - footerBlock.height

            // ===== Queue tab =====
            Flickable {
                visible: root.tab === 0
                anchors.fill: parent
                clip: true
                contentWidth: width
                contentHeight: queueBody.implicitHeight
                boundsBehavior: Flickable.StopAtBounds

                Column {
                    id: queueBody
                    width: parent.width
                    padding: 10
                    spacing: 12

                    // NOW PLAYING section.
                    Column {
                        visible: root.currentRow !== null
                        width: parent.width - 20
                        spacing: 10
                        Text {
                            text: QbzSession.tr("NOW PLAYING", QbzSession.trRev)
                            color: theme.textMuted
                            font.pixelSize: 11
                            font.weight: theme.weightSemibold
                            font.letterSpacing: 0.5
                        }
                        Rectangle {
                            width: parent.width
                            height: 44
                            radius: theme.radiusSm
                            // Ambient-aware: the one opaque card left in a translucent sidebar.
                            color: (theme.ambientOn ? theme.surfaceElevatedA50 : theme.surfaceElevated)
                            Row {
                                anchors.fill: parent
                                anchors.leftMargin: 6
                                anchors.rightMargin: 6
                                anchors.topMargin: 4
                                anchors.bottomMargin: 4
                                spacing: 9
                                Rectangle {
                                    width: 34
                                    height: 34
                                    radius: 4
                                    anchors.verticalCenter: parent.verticalCenter
                                    color: (theme.ambientOn ? theme.surfaceCardA50 : theme.surfaceCard)
                                    // No clip: RoundedImage self-confines.
                                    RoundedImage {
                                        anchors.fill: parent
                                        source: root.currentRow ? (root.coverMap[root.currentRow.artUrl] || "") : ""
                                        radius: 4
                                    }
                                    QbzSkeleton {
                                        variant: "art"
                                        anchors.fill: parent
                                        blockRadius: 4
                                        visible: root.currentRow
                                            ? root.coverPending(root.currentRow.artUrl) : false
                                        phase: root.skelPhase
                                        settleMs: root.artSettleMs
                                    }
                                }
                                Column {
                                    // A Row SKIPS invisible children, so the
                                    // heart's 28px + its 9px gap come back to
                                    // the text when it is hidden.
                                    width: parent.width - 34 - 9
                                        - (root.currentEphemeral ? 0 : 28 + 9)
                                    anchors.verticalCenter: parent.verticalCenter
                                    spacing: 2
                                    Text {
                                        width: parent.width
                                        text: root.currentRow ? root.currentRow.title : ""
                                        color: theme.textPrimary
                                        font.pixelSize: 12
                                        font.weight: theme.weightMedium
                                        elide: Text.ElideRight
                                    }
                                    Text {
                                        width: parent.width
                                        text: root.currentRow ? root.currentRow.artist : ""
                                        color: theme.textMuted
                                        font.pixelSize: 10
                                        elide: Text.ElideRight
                                    }
                                }
                                // Favorite toggle. HIDDEN on an ephemeral
                                // track: there is no DB row to favourite, and
                                // an ephemeral play must leave nothing behind
                                // (QueueSidebar.slint:563-565).
                                Rectangle {
                                    visible: !root.currentEphemeral
                                    width: 28
                                    height: 28
                                    radius: theme.radiusSm
                                    anchors.verticalCenter: parent.verticalCenter
                                    color: npFavArea.containsMouse ? theme.surfaceHover : "transparent"
                                    QbzIcon {
                                        anchors.centerIn: parent
                                        name: root.favState(root.currentRow) ? "heart-filled" : "heart"
                                        width: 17
                                        height: 17
                                        tintName: root.favState(root.currentRow) ? "favorite" : (npFavArea.containsMouse ? "textPrimary" : "muted")
                                    }
                                    MouseArea {
                                        id: npFavArea
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: root.toggleFav(root.currentRow)
                                    }
                                }
                            }
                        }
                    }

                    // UP NEXT section.
                    Column {
                        id: upNextSection
                        visible: (doc.upcomingTotal || 0) > 0
                        width: parent.width - 20
                        spacing: 8
                        Text {
                            text: QbzSession.tr("UP NEXT", QbzSession.trRev)
                            color: theme.textMuted
                            font.pixelSize: 11
                            font.weight: theme.weightSemibold
                            font.letterSpacing: 0.5
                        }

                        Repeater {
                            id: upcomingRepeater
                            model: root.upcoming
                            delegate: Column {
                                id: upDelegate
                                required property var modelData
                                required property int index
                                width: parent ? parent.width : 0
                                // Row center in window space — slotFromPointer()
                                // reads it off the Repeater's items.
                                function rowCenterY() { return qrow.rowCenterY() }
                                // #442 section header above the row.
                                Row {
                                    visible: modelData.section !== ""
                                    height: 22
                                    spacing: 6
                                    QbzIcon {
                                        visible: modelData.section === "next-in-queue"
                                        name: "list-start"
                                        width: 13
                                        height: 13
                                        anchors.verticalCenter: parent.verticalCenter
                                        tintName: "accent"
                                    }
                                    Text {
                                        anchors.verticalCenter: parent.verticalCenter
                                        text: modelData.section === "next-in-queue"
                                            ? QbzSession.tr("Next in queue", QbzSession.trRev) : QbzSession.tr("Next up", QbzSession.trRev)
                                        color: theme.textMuted
                                        font.pixelSize: 11
                                        font.weight: theme.weightSemibold
                                        font.letterSpacing: 0.5
                                    }
                                }
                                QueueRow {
                                    id: qrow
                                    width: parent.width
                                    row: modelData
                                    rowIndex: index
                                    showNumber: true
                                    inQueue: true
                                    reorderable: true
                                }
                            }
                        }

                        // Paginator (plain text controls, no pills).
                        Row {
                            visible: (doc.pageCount || 1) > 1
                            width: parent.width
                            anchors.horizontalCenter: parent.horizontalCenter
                            spacing: theme.spacingMd
                            Item { width: (parent.width - paginator.width) / 2; height: 1 }
                            Row {
                                id: paginator
                                spacing: theme.spacingMd
                                QbzIconButton { btnSize: 30
                                    name: "chevron-left"
                                    iconSize: 15
                                    btnEnabled: (doc.page || 0) > 0
                                    onClicked: QbzQueue.queueSetPage((doc.page || 0) - 1)
                                }
                                Text {
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: QbzSession.tr("Page {} of {}", QbzSession.trRev).replace("{}", (doc.page || 0) + 1).replace("{}", doc.pageCount || 1)
                                    color: theme.textMuted
                                    font.pixelSize: 12
                                }
                                QbzIconButton { btnSize: 30
                                    name: "chevron-right"
                                    iconSize: 15
                                    btnEnabled: (doc.page || 0) < (doc.pageCount || 1) - 1
                                    onClicked: QbzQueue.queueSetPage((doc.page || 0) + 1)
                                }
                            }
                            Item { width: (parent.width - paginator.width) / 2; height: 1 }
                        }
                    }

                    // Empty state — no current track and no upcoming.
                    Column {
                        visible: root.queueEmpty
                        width: parent.width - 20
                        topPadding: 48 - 10
                        spacing: 6
                        Text {
                            width: parent.width
                            text: QbzSession.tr("Your queue is empty", QbzSession.trRev)
                            color: theme.textSecondary
                            font.pixelSize: theme.fontBody
                            font.weight: theme.weightMedium
                            horizontalAlignment: Text.AlignHCenter
                        }
                        Text {
                            width: parent.width
                            text: QbzSession.tr("Play an album or track to get started", QbzSession.trRev)
                            color: theme.textMuted
                            font.pixelSize: 12
                            horizontalAlignment: Text.AlignHCenter
                            wrapMode: Text.WordWrap
                        }
                    }

                    // No-results state — a search is active but nothing
                    // matched. Distinct from the empty queue above: the queue
                    // still has tracks, they are just filtered out, so telling
                    // the user to "play an album to get started" would be a lie.
                    Text {
                        visible: root.noSearchResults
                        width: parent.width - 20
                        topPadding: 32 - 10
                        text: QbzSession.tr("No tracks match your search", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: 12
                        horizontalAlignment: Text.AlignHCenter
                    }
                }
            }

            // ===== History tab =====
            Flickable {
                visible: root.tab === 1
                anchors.fill: parent
                clip: true
                contentWidth: width
                contentHeight: historyBody.implicitHeight
                boundsBehavior: Flickable.StopAtBounds

                Column {
                    id: historyBody
                    width: parent.width
                    padding: 10
                    spacing: 8

                    Text {
                        text: QbzSession.tr("RECENTLY PLAYED", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: 11
                        font.weight: theme.weightSemibold
                        font.letterSpacing: 0.5
                    }
                    Repeater {
                        model: root.historyRows
                        delegate: QueueRow {
                            width: parent.width
                            row: modelData
                            rowIndex: index
                            showNumber: false
                            inQueue: false
                        }
                    }
                    Text {
                        visible: root.historyRows.length === 0
                        width: parent.width
                        topPadding: 48 - 10
                        text: QbzSession.tr("Nothing played yet", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: theme.fontBody
                        horizontalAlignment: Text.AlignHCenter
                    }
                }
            }
        }

        // --- Footer: count + actions + inline search (Queue tab) ---------
        Column {
            id: footerBlock
            visible: root.tab === 0 && !root.queueEmpty
            width: parent.width
            spacing: 0
            Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }
            Text {
                visible: (doc.upcomingTotal || 0) > 0
                width: parent.width
                leftPadding: theme.spacingMd
                rightPadding: theme.spacingMd
                topPadding: 8
                text: (doc.upcomingTotal === 1
                        ? QbzSession.tr("Showing {} of {} track.", QbzSession.trRev)
                        : QbzSession.tr("Showing {} of {} tracks.", QbzSession.trRev))
                      .replace("{}", doc.pageEnd || 0).replace("{}", doc.upcomingTotal || 0)
                color: theme.textMuted
                font.pixelSize: 11
            }
            Row {
                id: footerRow
                width: parent.width
                leftPadding: theme.spacingMd
                rightPadding: theme.spacingMd
                topPadding: 10
                bottomPadding: 10
                spacing: theme.spacingXs

                // The row's usable width — `parent.width` here is the PANEL
                // width, padding included; using it raw is what pushed the
                // search field out of the 300px column.
                readonly property int contentWidth:
                    width - leftPadding - rightPadding

                // Action 1 — Clear queue.
                QbzIconButton { btnSize: 30
                    name: "trash-list"
                    onClicked: QbzQueue.queueClear()
                }
                // Action 2 — Save the queue as a playlist: seeds the shared
                // Add-to-Playlist picker with the whole queue, whose inline
                // "Create new playlist" row is what names it. HIDDEN while an
                // ephemeral track plays — an ephemeral queue cannot be
                // persisted (QueueSidebar.slint:848-858).
                QbzIconButton { btnSize: 30
                    name: "add-to-list"
                    visible: !root.currentEphemeral
                    onClicked: QbzQueue.queueSaveAsPlaylist()
                }
                // Action 3 — infinite play (accent while armed).
                QbzIconButton { btnSize: 30
                    name: "infinity"
                    active: doc.infinitePlay === true
                    onClicked: QbzQueue.queueToggleInfinitePlay()
                }
                // Action 4 — sleep timer. Accent while armed; the popover
                // opens ABOVE the footer with the preset picker / countdown.
                QbzIconButton {
                    id: sleepBtn
                    btnSize: 30
                    name: "clock"
                    active: QbzQueue.sleepActive
                    onClicked: sleepMenu.opened ? sleepMenu.close() : sleepMenu.open()

                    Popup {
                        id: sleepMenu
                        // Anchored to the button's top-left and pushed up by
                        // its own height, so it grows upward out of the footer
                        // (the .slint hard-codes x:-188 y:-214 for the same
                        // effect; here the height is not a magic number).
                        x: -188
                        y: -height - 6
                        width: 220
                        padding: 0
                        // A click INSIDE must not dismiss it, or a preset
                        // could never be picked before Set is pressed — the
                        // same reason the .slint sets close-on-click-outside.
                        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutsideParent
                        background: Rectangle {
                            radius: theme.radiusMd
                            color: theme.surfaceCard
                            border.width: 1
                            border.color: theme.borderSubtle
                        }
                        // Local until Set is pressed: the preset choice is a
                        // draft, and the reference keeps it in UI state too.
                        property int selectedIndex: 0
                        property string customMinutes: "60"

                        contentItem: Column {
                            padding: 12
                            spacing: 10

                            // Armed: countdown + Cancel.
                            Column {
                                visible: QbzQueue.sleepActive
                                width: 196
                                spacing: 8
                                Text {
                                    text: QbzSession.tr("Stops in", QbzSession.trRev)
                                    color: theme.textMuted
                                    font.pixelSize: 11
                                }
                                Text {
                                    text: QbzQueue.sleepRemaining
                                    color: theme.accent
                                    font.pixelSize: 16
                                    font.weight: theme.weightSemibold
                                }
                                Rectangle {
                                    width: parent.width
                                    height: 32
                                    radius: theme.radiusSm
                                    border.width: 1
                                    border.color: theme.borderSubtle
                                    color: cancelArea.containsMouse ? theme.surfaceHover : theme.surfaceElevated
                                    Text {
                                        anchors.fill: parent
                                        text: QbzSession.tr("Cancel Timer", QbzSession.trRev)
                                        color: theme.textSecondary
                                        font.pixelSize: 12
                                        horizontalAlignment: Text.AlignHCenter
                                        verticalAlignment: Text.AlignVCenter
                                    }
                                    MouseArea {
                                        id: cancelArea
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: {
                                            QbzQueue.sleepTimerCancel()
                                            sleepMenu.close()
                                        }
                                    }
                                }
                            }

                            // Idle: preset picker (+ custom minutes) + Set.
                            Column {
                                visible: !QbzQueue.sleepActive
                                width: 196
                                spacing: 10
                                Text {
                                    text: QbzSession.tr("Stop playback after:", QbzSession.trRev)
                                    color: theme.textMuted
                                    font.pixelSize: 11
                                }
                                Column {
                                    width: parent.width
                                    spacing: 2
                                    Repeater {
                                        model: [
                                            QbzSession.tr("30 min", QbzSession.trRev),
                                            QbzSession.tr("1 hr", QbzSession.trRev),
                                            QbzSession.tr("2 hr", QbzSession.trRev),
                                            QbzSession.tr("3 hr", QbzSession.trRev),
                                            QbzSession.tr("5 hr", QbzSession.trRev),
                                            QbzSession.tr("Custom…", QbzSession.trRev)
                                        ]
                                        delegate: Rectangle {
                                            required property var modelData
                                            required property int index
                                            width: parent ? parent.width : 0
                                            height: 30
                                            radius: theme.radiusSm
                                            color: sleepMenu.selectedIndex === index
                                                ? theme.accent
                                                : (presetArea.containsMouse ? theme.surfaceHover : "transparent")
                                            Text {
                                                x: 10
                                                width: parent.width - 20
                                                height: parent.height
                                                text: modelData
                                                color: sleepMenu.selectedIndex === index
                                                    ? theme.accentText : theme.textPrimary
                                                font.pixelSize: 12
                                                verticalAlignment: Text.AlignVCenter
                                                elide: Text.ElideRight
                                            }
                                            MouseArea {
                                                id: presetArea
                                                anchors.fill: parent
                                                hoverEnabled: true
                                                cursorShape: Qt.PointingHandCursor
                                                onClicked: sleepMenu.selectedIndex = index
                                            }
                                        }
                                    }
                                }
                                // Custom minutes — revealed by the last preset.
                                // A bare TextInput in a styled Rectangle, the
                                // AddToMixtapeModal convention (and what the
                                // .slint does): QbzLineEdit has no numeric arm.
                                Rectangle {
                                    width: parent.width
                                    height: 32
                                    visible: sleepMenu.selectedIndex === 5
                                    radius: theme.radiusSm
                                    border.width: 1
                                    border.color: customInput.activeFocus ? theme.accent : theme.borderSubtle
                                    color: theme.surfaceElevated
                                    TextInput {
                                        QbzTextEditMenu { }
                                        id: customInput
                                        anchors.fill: parent
                                        anchors.leftMargin: 9
                                        anchors.rightMargin: 34
                                        color: theme.textPrimary
                                        font.pixelSize: 12
                                        verticalAlignment: Text.AlignVCenter
                                        clip: true
                                        selectByMouse: true
                                        // Digits only, so "Set" can never be
                                        // handed something `parseInt` reads as
                                        // NaN (the Rust side clamps to
                                        // [1, 1440] on top of this).
                                        validator: IntValidator { bottom: 1; top: 1440 }
                                        inputMethodHints: Qt.ImhDigitsOnly
                                        text: sleepMenu.customMinutes
                                        onTextEdited: sleepMenu.customMinutes = text
                                        Binding {
                                            target: customInput
                                            property: "text"
                                            value: sleepMenu.customMinutes
                                            when: !customInput.activeFocus
                                        }
                                    }
                                    Text {
                                        anchors.right: parent.right
                                        anchors.rightMargin: 9
                                        anchors.verticalCenter: parent.verticalCenter
                                        text: QbzSession.tr("min", QbzSession.trRev)
                                        color: theme.textMuted
                                        font.pixelSize: 11
                                    }
                                }
                                Rectangle {
                                    width: parent.width
                                    height: 34
                                    radius: theme.radiusSm
                                    color: setArea.containsMouse ? theme.accentHover : theme.accent
                                    Text {
                                        anchors.fill: parent
                                        text: QbzSession.tr("Set Timer", QbzSession.trRev)
                                        color: theme.accentText
                                        font.pixelSize: 12
                                        font.weight: theme.weightMedium
                                        horizontalAlignment: Text.AlignHCenter
                                        verticalAlignment: Text.AlignVCenter
                                    }
                                    MouseArea {
                                        id: setArea
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: {
                                            var mins = [30, 60, 120, 180, 300][sleepMenu.selectedIndex]
                                            if (mins === undefined)
                                                mins = parseInt(sleepMenu.customMinutes, 10)
                                            if (!isNaN(mins) && mins > 0)
                                                QbzQueue.sleepTimerSet(mins)
                                            sleepMenu.close()
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Filler so the collapsed magnifier sits at the right edge;
                // the field opens LEFT across it. One gap per visible control
                // (save-as-playlist disappears on an ephemeral queue).
                Item {
                    width: Math.max(0, footerRow.contentWidth
                                       - (root.currentEphemeral ? 3 : 4) * 30 - 30
                                       - (root.currentEphemeral ? 4 : 5) * theme.spacingXs)
                    height: 1
                }
                // Inline queue search (wired) — the shared expandable control.
                QbzLineEdit {
                    searchMode: true
                    expandable: true
                    sm: true
                    text: doc.searchQuery || ""
                    // Bound to the PANEL, never wider: it opens leftward over
                    // the four action buttons, exactly like the Slint
                    // ExpandableSearch does over its neighbours.
                    openWidth: Math.max(90, footerRow.contentWidth)
                    placeholder: QbzSession.tr("Search queue", QbzSession.trRev)
                    onEdited: function (v) { QbzQueue.queueSetSearch(v) }
                }
            }
        }
    }

    // Floating drag ghost (QueueSidebar.slint:1098-1130) — a pill following
    // the pointer with the dragged track's title, so it is clear something is
    // being moved. A free child of the panel ROOT (not of the Column, which
    // would stack it), declared LAST so it renders above the list, and with
    // no mouse target of its own. The live pointer window-Y is converted into
    // panel-local space here; mapToItem() is re-evaluated on every
    // reorderPointerY change, which is the only thing that moves it.
    Rectangle {
        visible: root.reorderActive && root.reorderFrom >= 0
            && root.reorderFrom < root.upcoming.length
        x: theme.spacingMd
        y: root.reorderPointerY - root.mapToItem(null, 0, 0).y - height / 2
        width: root.width - 2 * theme.spacingMd
        height: 34
        radius: theme.radiusSm
        color: Qt.rgba(theme.surfaceElevated.r, theme.surfaceElevated.g,
                       theme.surfaceElevated.b, 0.95)
        border.width: 1
        border.color: Qt.rgba(theme.accent.r, theme.accent.g, theme.accent.b, 0.95)
        // Alpha folded into the fill/border instead of a container `opacity`:
        // an opacity node is its own batch group, and this ghost is live for
        // the whole drag. Every theme builds these tokens opaque (checked
        // across crates/qbz-theme/src), so this is the same pixel.
        Text {
            anchors.fill: parent
            anchors.leftMargin: theme.spacingSm
            anchors.rightMargin: theme.spacingSm
            verticalAlignment: Text.AlignVCenter
            text: parent.visible ? (root.upcoming[root.reorderFrom].title || "") : ""
            color: theme.textPrimary
            font.pixelSize: 12
            font.weight: theme.weightMedium
            elide: Text.ElideRight
        }
    }

    // --- Empty-queue drop prompt (owner ask 2026-08-10) -------------------
    // A drop is an ADD, never a play — dropping onto a running queue must not
    // hijack it. But onto an EMPTY queue that leaves the user one click short
    // of what they meant, so the panel asks. A flyout, not a modal: the
    // question is low-stakes and belongs to this panel, and a full-window
    // scrim for "start playing?" would be heavier than the decision.
    //
    // Rides the panel's top edge, under the header, and takes no input away
    // from the list while hidden (`visible` gates the whole item).
    Rectangle {
        id: dropPlayPrompt
        visible: QbzQueue.dropPlayPrompt
        anchors.top: parent.top
        anchors.topMargin: 92
        anchors.horizontalCenter: parent.horizontalCenter
        width: parent.width - 32
        height: promptCol.implicitHeight + 24
        radius: theme.radiusMd
        color: theme.surfaceElevated
        border.width: 1
        border.color: theme.borderSubtle
        z: 50

        // Swallow clicks so a press on the prompt never reaches a queue row.
        MouseArea { anchors.fill: parent }

        Column {
            id: promptCol
            x: 12
            y: 12
            width: parent.width - 24
            spacing: 10
            Text {
                width: parent.width
                text: QbzSession.tr("Start playing now?", QbzSession.trRev)
                color: theme.textPrimary
                font.pixelSize: theme.fontBody
                font.weight: theme.weightMedium
                wrapMode: Text.WordWrap
            }
            Row {
                anchors.right: parent.right
                spacing: 8
                IconTextButton {
                    label: QbzSession.tr("Not now", QbzSession.trRev)
                    hasIcon: false
                    onClicked: QbzQueue.queueAnswerDropPrompt(false)
                }
                // Accent-filled primary, the QbzConfirmModal shape.
                Rectangle {
                    anchors.verticalCenter: parent.verticalCenter
                    width: playLabel.implicitWidth + 28
                    height: 32
                    radius: theme.radiusSm
                    color: playArea.containsMouse ? theme.accentHover : theme.accent
                    Text {
                        id: playLabel
                        anchors.centerIn: parent
                        text: QbzSession.tr("Play now", QbzSession.trRev)
                        color: theme.accentGlyphColor
                        font.pixelSize: 13
                        font.weight: theme.weightSemibold
                    }
                    MouseArea {
                        id: playArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: QbzQueue.queueAnswerDropPrompt(true)
                    }
                }
            }
        }
    }

    QbzTooltip {
        id: tips
        anchors.fill: parent
        z: 4000
    }
}
