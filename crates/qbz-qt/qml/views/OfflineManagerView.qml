// Offline Cache Manager — QML port of crates/qbz-ui/ui/offline/
// OfflineManagerView.slint. A FULL-PAGE content view (route "offlinemanager"),
// reached only from Settings > Offline > "Open manager", exactly like the
// Blacklist manager next door.
//
// Layout, 1:1 with the reference: a stats bar (totals · usage bar · GB limit
// field · open-folder · clear-all) over a toolbar (sort · failed-only) over a
// two-pane body — a 210px A-Z artist rail on the left, the album-header +
// track rows on the right.
//
// Everything comes out of ONE document, QbzOffline.managerJson. The rollup,
// the ordering, the sizes and the status integers are all Rust-side
// (src/offline_manager_qt.rs); QML never filters and never sorts.
//
// Deltas vs the Slint, each deliberate:
//
// - NO per-view Back chrome (the Slint's NavButtons row, :171). Nav history is
//   the global HeaderBar in this port — the same treatment
//   views/BlacklistManagerView.qml already documents for the identical case.
//   ADR-004 is satisfied by the HeaderBar.
// - The bulk actions are the SHARED controls/QbzMultiSelectBar.qml, not the
//   reference's inline `BulkIconBtn` row plus a separate select-all toggle.
//   The Slint's own comment (:115-118) says that component was "homologated
//   with MultiSelectBar's BulkButton ... so the offline-cache track rows share
//   the app-wide bulk-bar style" — this port HAS that bar, it already carries
//   both "Select all" and "Clear" as actions, and every other multi-select
//   surface here uses it.
// - Selection is QML-local (`selected` map below), the port's convention
//   (LocalLibraryView.qml:100) rather than the reference's in-place edits on
//   the Slint model. The rebuild is then a pure function of the DB, so a
//   download finishing mid-selection cannot fight the user's checkboxes.
// - Covers are file paths in the document, loaded by RoundedImage with
//   sourceSize; the reference pre-decodes pixels on its worker because
//   slint::Image is not Send. Qt has no such constraint.
//
// Live progress does NOT wait for a rebuild: QbzShell.trackCacheStatusChanged
// is the app-wide row-status channel and this view patches its own rows from
// it, the same way the album page and the track rows do. A rebuild only
// happens on a terminal mutation (offline_manager_qt::refresh_if_open).
//
// Nav re-entry: nav_qt::back()/forward() republish `currentView` and run NO
// per-view load, so this view calls QbzOffline.reload() in
// Component.onCompleted.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Rectangle {
    id: root
    property bool kioskHost: false

    // Transparent while the ambient background is active — the frosted content
    // panel shows through (HomeView.qml:53 and its twelve siblings).
    readonly property bool ambientOn: theme.ambientOn
    color: root.ambientOn ? "transparent" : theme.surfaceMain
    // Round to the AppShell content-frame bezel (Radius.md): QML clips are
    // rectangular, so the frame's rounding never reaches the view.
    radius: 12

    QbzTheme { id: theme }

    // ============================== data ==================================

    readonly property var doc: {
        try {
            return JSON.parse(QbzOffline.managerJson)
        } catch (e) {
            return ({})
        }
    }
    readonly property var rows: root.doc.rows || []
    readonly property var artists: root.doc.artists || []
    readonly property int tracksCount: root.doc.tracksCount || 0

    /// Checked TRACK rows, keyed by track id. Cleared on every publish: the
    /// rows may have been re-filtered or removed underneath, and a checkbox
    /// pointing at a row that is gone is a bulk action on nothing.
    property var selected: ({})
    readonly property int selectedCount: Object.keys(root.selected).length
    onDocChanged: root.selected = ({})

    /// Live per-row STATUS patches (id -> {status}) merged over the document.
    /// Kept separate so the next publish drops them wholesale rather than
    /// leaving a stale spinner behind.
    ///
    /// The signal also carries a progress fraction and the document carries
    /// `progress` per row, but neither is drawn — the reference does not draw
    /// it either: its StatusIcon spins, it does not fill. Both are kept in the
    /// payload because the row-status channel is app-wide and its shape is not
    /// this view's to trim.
    property var livePatch: ({})

    Component.onCompleted: QbzOffline.reload()

    Connections {
        target: QbzShell
        function onTrackCacheStatusChanged(trackId, status, progress) {
            var p = Object.assign({}, root.livePatch)
            p[trackId] = { "status": status, "progress": progress }
            root.livePatch = p
        }
    }

    function rowStatus(row) {
        var p = root.livePatch[row.trackId]
        return (row.kind === "track" && p !== undefined) ? p.status : row.status
    }
    function toggleSelect(trackId) {
        var s = Object.assign({}, root.selected)
        if (s[trackId] === true)
            delete s[trackId]
        else
            s[trackId] = true
        root.selected = s
    }

    function bulkAction(id) {
        if (id === "clear") {
            root.selected = ({})
            return
        }
        if (id === "select-all") {
            var s = ({})
            for (var i = 0; i < root.rows.length; i++)
                if (root.rows[i].kind === "track") s[root.rows[i].trackId] = true
            root.selected = s
            return
        }
        var ids = Object.keys(root.selected)
        if (ids.length === 0)
            return
        if (id === "redownload") QbzOffline.bulkRedownload(JSON.stringify(ids))
        else if (id === "remove") QbzOffline.bulkRemove(JSON.stringify(ids))
        root.selected = ({})
    }

    // ============================== chrome ================================

    // The cache-status glyph. Status 2 (downloading) is the only animated one
    // and it rides the SHARED shell pulse — never a local Timer or
    // NumberAnimation (the repaint-pulse law: every continuous animator ticks
    // on QbzShell.pulseMs or the whole window presents at display rate).
    component StatusGlyph: Item {
        id: glyph
        property int status: 0
        width: 18
        height: 18

        property real spin: 0
        Connections {
            target: QbzShell
            enabled: glyph.status === 2 && glyph.visible
            function onPulseMsChanged() { glyph.spin = (glyph.spin + 12) % 360 }
        }

        QbzIcon {
            anchors.centerIn: parent
            width: 16
            height: 16
            rotation: glyph.status === 2 ? glyph.spin : 0
            name: glyph.status === 2 ? "loader-circle"
                : glyph.status === 3 ? "circle-check-big"
                : glyph.status === 4 ? "triangle-alert" : "cloud-download"
            tintName: glyph.status === 2 ? "accent"
                : glyph.status === 3 ? "accent"
                : glyph.status === 4 ? "warning" : "muted"
        }
    }

    // A 30px ghost icon button (the reference's IconBtn).
    component GhostBtn: Rectangle {
        id: gb
        property string name: ""
        property string tint: "muted"
        /// Hover tooltip text, shown by the view's QbzTooltip overlay ("" =
        /// none). Icon-only buttons get one; the row glyphs have their labels.
        property string tip: ""
        signal clicked()
        width: root.kioskHost ? 44 : 30
        height: root.kioskHost ? 44 : 30
        radius: theme.radiusSm
        color: gbArea.containsMouse ? theme.surfaceElevated : "transparent"
        QbzIcon {
            anchors.centerIn: parent
            width: 16
            height: 16
            name: gb.name
            tintName: gbArea.containsMouse ? "textPrimary" : gb.tint
        }
        MouseArea {
            id: gbArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: {
                tips.hide("ghost-" + gb.name)
                gb.clicked()
            }
            onContainsMouseChanged: tips.hover(containsMouse, gb, "ghost-" + gb.name, gb.tip)
        }
    }

    // ============================== layout ================================

    Column {
        anchors.fill: parent
        anchors.margins: root.kioskHost ? 8 : 24
        spacing: root.kioskHost ? 6 : 14

        // ---- Header ------------------------------------------------------
        Text {
            text: QbzSession.tr("Offline Cache", QbzSession.trRev)
            color: theme.textPrimary
            font.pixelSize: theme.fontSection
            font.weight: theme.weightBold
        }

        // ---- Stats bar ---------------------------------------------------
        Rectangle {
            width: parent.width
            visible: !root.kioskHost || root.selectedCount === 0
            height: root.kioskHost ? 44 : 76
            radius: theme.radiusMd
            color: theme.surfaceElevated

            // Anchored, NOT a Row: the usage bar has to take exactly the
            // slack between the two fixed clusters, and expressing that as a
            // width subtraction inside a Row needs a named constant for the
            // left cluster — which is how a phantom spacer item ends up in the
            // layout adding its own width.
            Column {
                id: statsLeft
                anchors.left: parent.left
                anchors.leftMargin: 16
                anchors.verticalCenter: parent.verticalCenter
                spacing: 2
                Text {
                    text: root.doc.tracksText || ""
                    color: theme.textPrimary
                    font.pixelSize: theme.fontBody
                    font.weight: theme.weightSemibold
                }
                Text {
                    text: (root.doc.sizeText || "") + " " + (root.doc.limitText || "")
                    color: theme.textMuted
                    font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
                }
            }

            Row {
                id: statsRight
                anchors.right: parent.right
                anchors.rightMargin: 16
                anchors.verticalCenter: parent.verticalCenter
                spacing: 6

                QbzLineEdit {
                    kioskHost: root.kioskHost
                    id: limitField
                    anchors.verticalCenter: parent.verticalCenter
                    width: 58
                    // Seeded from the document, then owned locally while the
                    // user types — binding it to `limitGb` would put the
                    // persisted number back under the cursor on every
                    // republish (the re-seed rule in QbzLineEdit.qml:67).
                    text: (root.doc.limitGb || 5).toString()
                    onAccepted: function (v) { QbzOffline.setLimit(parseInt(v) || 0) }
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: "GB"
                    color: theme.textMuted
                    font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
                }
                GhostBtn {
                    anchors.verticalCenter: parent.verticalCenter
                    name: "check"
                    tip: QbzSession.tr("Apply the size limit", QbzSession.trRev)
                    onClicked: QbzOffline.setLimit(parseInt(limitField.text) || 0)
                }
                GhostBtn {
                    anchors.verticalCenter: parent.verticalCenter
                    name: "folder"
                    tip: QbzSession.tr("Open the cache folder", QbzSession.trRev)
                    onClicked: QbzOffline.openFolder()
                }
                // The ONE place the whole offline cache can be purged (the
                // Settings row that mirrored it was dropped 2026-09-13): a
                // tooltip says so before the click, the modal says what goes.
                GhostBtn {
                    anchors.verticalCenter: parent.verticalCenter
                    name: "trash-2"
                    tint: "warning"
                    tip: QbzSession.tr("Clear the whole offline cache…", QbzSession.trRev)
                    onClicked: clearConfirm.open()
                }
            }

            Rectangle {
                anchors.left: statsLeft.right
                anchors.right: statsRight.left
                anchors.leftMargin: 20
                anchors.rightMargin: 20
                anchors.verticalCenter: parent.verticalCenter
                height: 8
                radius: 4
                color: theme.surfaceMain
                Rectangle {
                    width: parent.width * Math.max(0, Math.min(1, root.doc.usage || 0))
                    height: parent.height
                    radius: 4
                    color: (root.doc.usage || 0) >= 1.0 ? theme.danger : theme.accent
                }
            }
        }

        // ---- Toolbar -----------------------------------------------------
        Row {
            width: parent.width
            height: root.kioskHost ? 44 : 32
            spacing: 12

            QbzSelect {
                    kioskHost: root.kioskHost
                anchors.verticalCenter: parent.verticalCenter
                menuWidth: 160
                sm: true
                options: [
                    QbzSession.tr("Sort: A-Z", QbzSession.trRev),
                    QbzSession.tr("Sort: Recent", QbzSession.trRev),
                    QbzSession.tr("Sort: Largest", QbzSession.trRev),
                    QbzSession.tr("Sort: Smallest", QbzSession.trRev)
                ]
                currentIndex: root.doc.sortIndex || 0
                onSelected: function (i) { QbzOffline.setSort(i) }
            }

            Rectangle {
                anchors.verticalCenter: parent.verticalCenter
                width: failedLabel.implicitWidth + 28
                height: root.kioskHost ? 44 : 32
                radius: theme.radiusSm
                border.width: 1
                border.color: root.doc.showOnlyFailed === true ? theme.accent : theme.borderSubtle
                color: root.doc.showOnlyFailed === true
                    ? theme.dangerBg
                    : (failedArea.containsMouse ? theme.surfaceElevated : "transparent")
                Text {
                    id: failedLabel
                    x: 14
                    anchors.verticalCenter: parent.verticalCenter
                    text: QbzSession.tr("Failed only", QbzSession.trRev)
                    color: root.doc.showOnlyFailed === true ? theme.danger : theme.textSecondary
                    font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
                }
                MouseArea {
                    id: failedArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: QbzOffline.toggleFailed()
                }
            }
        }

        // ---- Body --------------------------------------------------------
        Item {
            width: parent.width
            height: parent.height - y

            Row {
                anchors.fill: parent
                anchors.bottomMargin: root.selectedCount > 0 ? 58 : 0
                spacing: root.kioskHost ? 6 : 14

                // Left A-Z artist rail.
                Rectangle {
                    width: root.kioskHost ? 160 : 210
                    height: parent.height
                    radius: theme.radiusMd
                    color: theme.surfaceCard
                    clip: true

                    ListView {
                        id: railList
                        anchors.fill: parent
                        anchors.margins: 6
                        spacing: 2
                        // +1 for the "All artists" row, which is not in the
                        // document (it is the absence of a filter).
                        model: root.artists.length + 1
                        boundsBehavior: Flickable.StopAtBounds
                        // Row 0 is "All artists" — the ABSENCE of a filter, so
                        // it is not in the document and the model is one longer
                        // than the artist list. Every child reads the delegate
                        // through its own id: a `parent.parent` chain here
                        // silently resolves to the Column once anything is
                        // nested one level deeper.
                        delegate: Rectangle {
                            id: railRow
                            required property int index
                            readonly property bool isAll: index === 0
                            readonly property var a: isAll ? null : root.artists[index - 1]
                            readonly property bool active: isAll
                                ? (root.doc.selectedArtist || "") === ""
                                : (a !== null && a.selected === true)
                            width: railList.width
                            height: root.kioskHost ? 64 : 48
                            radius: 8
                            color: railRow.active ? theme.surfaceElevated
                                : (railArea.containsMouse ? theme.surfaceHover : "transparent")
                            Column {
                                anchors.verticalCenter: parent.verticalCenter
                                anchors.left: parent.left
                                anchors.leftMargin: 12
                                anchors.right: parent.right
                                anchors.rightMargin: 8
                                spacing: 2
                                Text {
                                    width: parent.width
                                    text: railRow.isAll
                                        ? QbzSession.tr("All artists", QbzSession.trRev)
                                        : railRow.a.name
                                    color: railRow.active ? theme.accent : theme.textPrimary
                                    font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
                                    font.weight: railRow.active
                                        ? theme.weightSemibold : theme.weightRegular
                                    elide: Text.ElideRight
                                }
                                Text {
                                    width: parent.width
                                    text: railRow.isAll
                                        ? (root.doc.tracksText || "")
                                        : railRow.a.meta
                                    color: theme.textMuted
                                    font.pixelSize: root.kioskHost ? 13 : 11
                                    elide: Text.ElideRight
                                }
                            }
                            MouseArea {
                                id: railArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: QbzOffline.selectArtist(
                                    railRow.isAll ? "" : railRow.a.name)
                            }
                        }
                        QbzScrollBar {
                            target: railList
                            anchors.right: parent.right
                            anchors.top: parent.top
                            anchors.bottom: parent.bottom
                        }
                    }
                }

                // Right list — the three mutually-exclusive body branches, in
                // the reference's order: loading · empty · the list.
                Item {
                    width: parent.width - (root.kioskHost ? 160 + 6 : 210 + 14)
                    height: parent.height

                    QbzSpinner {
                        anchors.centerIn: parent
                        visible: QbzOffline.managerLoading
                    }

                    Text {
                        anchors.centerIn: parent
                        visible: !QbzOffline.managerLoading && root.rows.length === 0
                        text: QbzSession.tr("No offline tracks yet", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: theme.fontBody
                    }

                    ListView {
                        id: rowList
                        anchors.fill: parent
                        visible: !QbzOffline.managerLoading && root.rows.length > 0
                        spacing: 2
                        clip: true
                        model: root.rows
                        boundsBehavior: Flickable.StopAtBounds
                        // A cache that can hold thousands of tracks is born
                        // windowed (TRACK-RULES §6) — never a Flickable over
                        // every row.
                        cacheBuffer: 400

                        delegate: Rectangle {
                            id: rowItem
                            required property var modelData
                            readonly property bool isAlbum: modelData.kind === "album"
                            readonly property bool checked:
                                root.selected[modelData.trackId] === true
                            width: rowList.width
                            height: root.kioskHost ? 64 : (isAlbum ? 60 : 44)
                            radius: 8
                            color: isAlbum ? theme.surfaceCard
                                : (rowArea.containsMouse ? theme.surfaceHover : "transparent")

                            MouseArea {
                                id: rowArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: rowItem.isAlbum
                                    ? Qt.ArrowCursor : Qt.PointingHandCursor
                                onClicked: if (!rowItem.isAlbum)
                                    QbzOffline.playTrack(rowItem.modelData.trackId)
                            }

                            Row {
                                anchors.fill: parent
                                anchors.leftMargin: rowItem.isAlbum ? 10 : 16
                                anchors.rightMargin: 12
                                spacing: 12

                                // Checkbox (track rows) / spacer (albums).
                                Item {
                                    width: root.kioskHost ? 44 : 16
                                    height: parent.height
                                    QbzCheckbox {
                    kioskHost: root.kioskHost
                                        anchors.centerIn: parent
                                        visible: !rowItem.isAlbum
                                        checked: rowItem.checked
                                        onToggled: root.toggleSelect(rowItem.modelData.trackId)
                                    }
                                }

                                // Leading — album cover or track number.
                                Item {
                                    width: 40
                                    height: parent.height
                                    Rectangle {
                                        anchors.centerIn: parent
                                        visible: rowItem.isAlbum
                                        width: 40
                                        height: 40
                                        radius: 4
                                        color: theme.surfaceMain
                                        Loader {
                                            anchors.fill: parent
                                            active: root.kioskHost && rowItem.isAlbum
                                            source: active ? "../kiosk/KioskArtwork.qml" : ""
                                            onLoaded: item.source = Qt.binding(function() {
                                                return rowItem.modelData.cover ? "file://" + rowItem.modelData.cover : ""
                                            })
                                        }
                                        RoundedImage {
                                            visible: !root.kioskHost
                                            anchors.fill: parent
                                            radius: 4
                                            source: !root.kioskHost && (rowItem.modelData.cover || "") !== ""
                                                ? "file://" + rowItem.modelData.cover : ""
                                        }
                                    }
                                    Text {
                                        anchors.centerIn: parent
                                        visible: !rowItem.isAlbum
                                        text: rowItem.modelData.number || ""
                                        color: theme.textMuted
                                        font.pixelSize: theme.fontBody
                                    }
                                }

                                // Title + subtitle.
                                Column {
                                    width: parent.width - (root.kioskHost ? 44 : 16) - 40 - metaText.width
                                        - (root.kioskHost ? 44 : 30) * 2 - 18 - 12 * 6
                                    anchors.verticalCenter: parent.verticalCenter
                                    spacing: 2
                                    Text {
                                        width: parent.width
                                        text: rowItem.modelData.title || ""
                                        color: theme.textPrimary
                                        font.pixelSize: rowItem.isAlbum
                                            ? theme.fontBody : theme.fontLegal
                                        font.weight: rowItem.isAlbum
                                            ? theme.weightSemibold : theme.weightRegular
                                        elide: Text.ElideRight
                                    }
                                    Text {
                                        width: parent.width
                                        text: rowItem.modelData.subtitle || ""
                                        color: theme.textMuted
                                        font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
                                        elide: Text.ElideRight
                                    }
                                }

                                Text {
                                    id: metaText
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: rowItem.modelData.meta || ""
                                    color: theme.textMuted
                                    font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
                                    horizontalAlignment: Text.AlignRight
                                }

                                GhostBtn {
                                    anchors.verticalCenter: parent.verticalCenter
                                    name: "refresh-cw"
                                    onClicked: rowItem.isAlbum
                                        ? QbzOffline.redownloadAlbum(rowItem.modelData.albumId)
                                        : QbzOffline.redownloadTrack(rowItem.modelData.trackId)
                                }
                                GhostBtn {
                                    anchors.verticalCenter: parent.verticalCenter
                                    name: "trash-2"
                                    onClicked: rowItem.isAlbum
                                        ? QbzOffline.removeAlbum(rowItem.modelData.albumId)
                                        : QbzOffline.removeTrack(rowItem.modelData.trackId)
                                }
                                StatusGlyph {
                                    anchors.verticalCenter: parent.verticalCenter
                                    status: root.rowStatus(rowItem.modelData)
                                }
                            }
                        }

                        // Back/forward scroll memory (controls/ScrollMemory.qml): reports
                        // this container's offset while it is the live page, and restores it
                        // when a back/forward step arms this route.
                        ScrollMemory { target: rowList; scope: "offlinemanager" }
                        QbzScrollBar {
                            target: rowList
                            anchors.right: parent.right
                            anchors.top: parent.top
                            anchors.bottom: parent.bottom
                        }
                    }
                }
            }

            // Bulk bar — the shared component, mounted the way every other
            // multi-select surface in this port mounts it.
            QbzMultiSelectBar {
                    kioskHost: root.kioskHost
                visible: root.selectedCount > 0
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                selectedCount: root.selectedCount
                actions: [
                    { "id": "select-all", "label": QbzSession.tr("Select all", QbzSession.trRev), "icon": "square-check-big", "danger": false, "needsSelection": false },
                    { "id": "redownload", "label": QbzSession.tr("Re-download", QbzSession.trRev), "icon": "refresh-cw", "danger": false, "needsSelection": true },
                    { "id": "remove", "label": QbzSession.tr("Remove", QbzSession.trRev), "icon": "trash-2", "danger": true, "needsSelection": true },
                    { "id": "clear", "label": QbzSession.tr("Clear", QbzSession.trRev), "icon": "x", "danger": false, "needsSelection": true }
                ]
                onAction: function (id) { root.bulkAction(id) }
            }
        }
    }

    // Clear-all confirmation. The Slint fires it straight from the trash glyph;
    // this port asks first. Purging the cache can be an hours-long re-download
    // and there is no undo, so the body says exactly what goes (the live
    // count and size from the stats bar) and what stays (purchased downloads
    // in the music folder — a different tree, never touched by clear_all).
    QbzConfirmModal {
        kioskHost: root.kioskHost
        id: clearConfirm
        // Fills the VIEW, like BlacklistManagerView.qml:523 — without it the
        // modal has no size and its scrim covers nothing.
        anchors.fill: parent
        title: QbzSession.tr("Clear the offline cache?", QbzSession.trRev)
        body: QbzSession.tr("Removes everything saved for offline listening: {} using {}. Purchased downloads in your music folder are kept; anything cleared has to be downloaded again to play offline.", QbzSession.trRev)
            .replace("{}", root.doc.tracksText || "")
            .replace("{}", root.doc.sizeText || "")
        confirmLabel: QbzSession.tr("Clear all", QbzSession.trRev)
        danger: true
        onConfirmed: QbzOffline.clearAll()
    }

    // Hover tooltips for the icon-only stats-bar buttons. QbzTooltip is the
    // shared overlay; it takes no pointer and owns no animator, so an idle
    // one costs nothing (ArtistSceneView.qml mounts it the same way). Below
    // the modal's z (ADR-009), so an open confirmation always covers it.
    QbzTooltip {
        id: tips
        anchors.fill: parent
        z: 900
    }
}
