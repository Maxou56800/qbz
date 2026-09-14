// LibraryArtistsPanel — the Artists tab's SIDEPANEL mode
// (FavoritesView.slint:2014-2185): a 300px A-Z master rail of the favourite
// artists on the left, the selected artist's releases on the right, each
// column scrolling on its own.
//
// LEFT is derived here from the rows the Library feed already carries — no
// fetch. RIGHT is `QbzLibrary.sidepanelJson` (src/library_sidepanel.rs), whose
// sections come from the SAME `/artist/page` classifier the full artist page
// uses, so the two can never disagree about what an artist released. Its five
// states are the reference's: nothing selected · loading · error · no albums ·
// the sections + a "Go to artist page" footer.
//
// The rail row is written here rather than reusing views/local/LocalArtistRow:
// that one is 60px, subtitles "N albums · M tracks" and reads its avatar gate
// off LocalLibraryView. The reference agrees — it declares its own
// `SidepanelArtistRow` (:218) beside the local one for exactly these reasons.

import QtQuick
import com.blitzfc.qbz
import "../../cards"
import "../../controls"
import "../../theme"

Item {
    id: root

    /// The LibraryView root (visibleRows, artMap, skelPhase).
    property var view: null

    QbzTheme { id: theme }

    readonly property int railW: 300
    readonly property int rowH: 56
    readonly property int headerH: 26

    // ------------------------------- left rail ---------------------------
    // Entry model — { t: 0, label } | { t: 1, item } — so ONE ListView
    // windows both the letter headers and the rows (the LocalArtistsTab
    // shape), instead of a Repeater per section.
    property var entries: []
    property var alphaJumps: []
    // The SAME array, indexed the same way, in the shape LibraryView's
    // `reportWindow` reads (`kind` / `artKey` / `imageUrl`): rail rows are the
    // feed row objects themselves, letter headers are the group-header
    // sentinel that report already knows to skip. It exists because the rail's
    // order is NOT `visibleRows` (it re-sorts A-Z and interleaves headers), so
    // index N here is not `visibleRows[N]` and the band has to travel with its
    // own array — see LibraryView.queueWindowReport's third argument.
    property var railRows: []
    readonly property var headerRow: ({ "kind": "group-header", "artKey": "", "imageUrl": "" })
    /// Held while `entries` and `railRows` are being swapped. Assigning
    /// `entries` re-evaluates the ListView's `model` binding SYNCHRONOUSLY,
    /// which emits `onModelChanged` -> `report()` one statement BEFORE
    /// `railRows` is updated — an index band read off the new entries against
    /// the old flat array. The trailing `report()` supersedes it in the common
    /// case (the debounce is a `restart()`), but NOT when the rebuild ends
    /// empty: `report()` bails on `railRows.length === 0`, leaving the stale
    /// band queued and pinning covers for artists the rail no longer shows
    /// (the "search with no matches" path). One flag closes both.
    property bool _rebuilding: false
    function rebuild() {
        var rows = root.view ? root.view.visibleRows : []
        var artists = []
        for (var i = 0; i < rows.length; i++)
            if (rows[i].kind === "artist") artists.push(rows[i])
        // Preserve the chosen sort; only alphabetical modes need letter headers.
        var mode = root.view.artistsSort
        var grouped = mode === "default" || mode === "default-reverse"
            || mode.indexOf("title-") === 0
        if (mode === "default" || mode === "default-reverse") {
            artists.sort(function(a, b) {
                var result = (a.title || "").localeCompare(b.title || "")
                return mode === "default-reverse" ? -result : result
            })
        }
        var out = []
        var flat = []
        var jumps = []
        var last = null
        for (i = 0; i < artists.length; i++) {
            var key = root.view.alphaKey(artists[i].title)
            if (grouped && key !== last) {
                jumps.push({ "letter": key, "index": out.length })
                out.push({ "t": 0, "label": key })
                flat.push(root.headerRow)
                last = key
            }
            out.push({ "t": 1, "item": artists[i] })
            flat.push(artists[i])
        }
        root._rebuilding = true
        root.entries = out
        root.railRows = flat
        root.alphaJumps = jumps
        root._rebuilding = false
        root.report()
    }
    // The rail's own keyed model (2026-09-14): an artist unfollowed from the
    // pane fades out of the rail while the rows below slide up, instead of
    // the rail being rebuilt from the top. Same page identity as the host's
    // bodies, so a search or a sort still replaces it at once.
    QbzKeyedModel {
        id: railModel
        rows: root.entries
        keyOf: function (entry) {
            return entry.t === 0 ? "letter:" + entry.label : "artist:" + entry.item.id
        }
        scope: root.view ? root.view.rowsScope : ""
        onReconciled: root.report()
    }

    Component.onCompleted: {
        railModel.views = [railList]
        root.rebuild()
        root.reportSoon()
    }
    Connections {
        target: root.view
        function onVisibleRowsChanged() { root.rebuild() }
    }

    // ------------------------ windowed avatars ---------------------------
    // The rail rides the SAME id-keyed artwork window every other Library
    // body uses — LibraryView.reportWindow -> QbzLibrary.libraryArtworkWindow
    // -> main.rs library_artwork_window -> libraryArtworkReady -> `artMap`,
    // which is what the row avatars bind to below. Until this landed the
    // panel reported nothing at all, so `artMap` only ever held whatever
    // another tab had left in it and practically every rail avatar drew the
    // placeholder glyph.
    //
    // Only the mounted band is asked for (rule 6): the rail is one row per
    // followed artist and can run to hundreds.
    function report() {
        if (root._rebuilding) return
        if (!root.view || root.railRows.length === 0) return
        // Same gate the sibling rail uses (views/local/LocalArtistsTab.qml:309)
        // and the host's own bodies now use: a hidden surface must not report,
        // because `artMap` is ONE map and a report PRUNES outside its band.
        // `contentY`/`height` keep firing while the whole Library view is off
        // screen (another nav destination), and a report made then evicts the
        // covers of whatever IS on screen. The `onVisibleChanged` handler on
        // `railList` re-reports when the rail comes back.
        if (!railList.visible) return
        var first = railList.indexAt(4, railList.contentY + 1)
        var last = railList.indexAt(4, railList.contentY + Math.max(1, railList.height) - 1)
        if (first < 0) first = 0
        if (last < 0) last = Math.min(root.railRows.length - 1, first + 12)
        root.view.queueWindowReport(Math.max(0, first - 4),
                                    Math.min(root.railRows.length - 1, last + 4),
                                    root.railRows)
    }
    /// Report now, then again once the ListView has laid out — `indexAt`
    /// answers -1 before that, which is the state of a rail that has just been
    /// built or just been mounted (views/local/LocalArtistsTab.qml:330).
    Timer {
        id: reportSettle
        interval: 50
        repeat: false
        onTriggered: root.report()
    }
    function reportSoon() {
        root.report()
        reportSettle.restart()
    }
    // Leaving the mode drops the selection, so re-entering does not paint a
    // stale artist's discography under a rail with nothing highlighted.
    Component.onDestruction: QbzLibrary.sidepanelClear()

    // ------------------------------ right pane ---------------------------
    readonly property var panel: {
        try {
            return JSON.parse(QbzLibrary.sidepanelJson)
        } catch (e) {
            return {}
        }
    }
    readonly property string selectedId: panel.artistId || ""

    // Right-pane covers are URL-keyed, exactly like the artist page's
    // (views/ArtistView.qml): `QbzShell.sidebarArtworkWindow` asks for a list
    // of urls and `QbzLibrary.libraryArtworkReady` answers with (url, path).
    // Arrivals are coalesced into ONE rebind per frame — per-arrival rebinding
    // is O(n²) over a big discography.
    property var coverMap: ({})
    property var _coverInbox: ({})
    Timer {
        id: coverFlush
        interval: 16
        repeat: false
        onTriggered: {
            var m = Object.assign({}, root.coverMap, root._coverInbox)
            root._coverInbox = ({})
            root.coverMap = m
        }
    }
    Connections {
        target: QbzLibrary
        function onLibraryArtworkReady(key, path) {
            root._coverInbox[key] = path
            if (!coverFlush.running) coverFlush.start()
        }
    }
    onPanelChanged: {
        var urls = []
        var sections = root.panel.sections || []
        for (var s = 0; s < sections.length; s++) {
            var albums = sections[s].albums || []
            for (var i = 0; i < albums.length; i++) {
                var u = albums[i].artUrl || ""
                if (u !== "" && !root.coverMap[u]) urls.push(u)
            }
        }
        if (urls.length > 0) QbzShell.sidebarArtworkWindow(JSON.stringify(urls))
    }

    // =========================== layout ==================================
    Item {
        id: rail
        x: 32
        y: 16
        width: root.railW
        height: parent.height - 16
        clip: true

        ListView {
            id: railList
            anchors.fill: parent
            anchors.rightMargin: 22
            clip: true
            cacheBuffer: 56 * 8
            reuseItems: true
            boundsBehavior: Flickable.StopAtBounds
            model: railModel
            add: QbzRowAdd { enabled: railModel.animate }
            remove: QbzRowRemove { enabled: railModel.animate }
            move: QbzRowDisplaced { enabled: railModel.animate }
            displaced: QbzRowDisplaced { enabled: railModel.animate }

            // The trigger set views/local/LocalArtistsTab.qml:161-165 already
            // uses for the same shape of rail: scroll, model swap, viewport
            // resize, and a settle pass when the pane becomes visible (a
            // report made against a ListView that has not laid out gets -1
            // from `indexAt`, and nothing re-fires once it has).
            onContentYChanged: root.report()
            onModelChanged: root.report()
            onHeightChanged: root.report()
            onVisibleChanged: if (visible) root.reportSoon()

            delegate: Loader {
                required property string rowKey
                required property int rowRev
                readonly property var modelData: railModel.row(rowKey, rowRev)
                ListView.onReused: { opacity = 1; scale = 1 }
                width: railList.width
                height: modelData.t === 0 ? root.headerH : root.rowH
                sourceComponent: modelData.t === 0 ? letterComp : artistComp

                Component {
                    id: letterComp
                    Item {
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: modelData.label
                            color: theme.textSecondary
                            font.pixelSize: theme.fontLegal
                            font.weight: theme.weightSemibold
                        }
                    }
                }
                Component {
                    id: artistComp
                    Rectangle {
                        id: railRow
                        readonly property bool picked: root.selectedId === modelData.item.id
                        width: railList.width
                        height: root.rowH
                        radius: 8
                        color: railRow.picked ? theme.surfaceElevated
                             : railRowArea.containsMouse ? theme.surfaceHover : "transparent"
                        Row {
                            anchors.fill: parent
                            anchors.leftMargin: 8
                            anchors.rightMargin: 8
                            spacing: 10
                            Rectangle {
                                width: 40
                                height: 40
                                anchors.verticalCenter: parent.verticalCenter
                                radius: 20
                                color: theme.surfaceElevated
                                clip: true
                                RoundedImage {
                                    anchors.fill: parent
                                    source: root.view.artMap[modelData.item.artKey] || ""
                                    radius: 20
                                }
                                QbzIcon {
                                    visible: (root.view.artMap[modelData.item.artKey] || "") === ""
                                    name: "user"
                                    width: 18
                                    height: 18
                                    anchors.centerIn: parent
                                    tintName: "muted"
                                }
                            }
                            Text {
                                width: parent.width - 40 - 10
                                height: parent.height
                                text: modelData.item.title || ""
                                color: railRow.picked ? theme.textPrimary : theme.textSecondary
                                font.pixelSize: theme.fontBody
                                font.weight: railRow.picked ? theme.weightSemibold : theme.weightRegular
                                verticalAlignment: Text.AlignVCenter
                                elide: Text.ElideRight
                            }
                        }
                        MouseArea {
                            id: railRowArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: QbzLibrary.sidepanelSelectArtist(
                                modelData.item.id, modelData.item.title || "")
                        }
                    }
                }
            }
        }
        QbzAlphaStrip {
            visible: root.entries.length > 0
            anchors.right: parent.right
            anchors.top: parent.top
            anchors.bottom: parent.bottom
            jumps: root.alphaJumps
            completeAlphabet: true
            onJump: function (ordinal, index) {
                railList.positionViewAtIndex(index, ListView.Beginning)
            }
        }
    }

    Rectangle {
        x: rail.x + rail.width
        y: 16
        width: 1
        height: parent.height - 16
        color: theme.borderSubtle
    }

    Item {
        id: pane
        x: rail.x + rail.width + 1
        y: 16
        width: parent.width - x - 32
        height: parent.height - 16
        clip: true

        // 1 — nothing selected.
        Column {
            visible: root.selectedId === ""
            anchors.centerIn: parent
            spacing: 10
            QbzIcon {
                anchors.horizontalCenter: parent.horizontalCenter
                name: "user"
                width: 44
                height: 44
                tintName: "muted"
            }
            Text {
                anchors.horizontalCenter: parent.horizontalCenter
                text: QbzSession.tr("Select an artist to see their albums.", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: theme.fontBody
            }
        }
        // 2 — loading.
        QbzSpinner {
            visible: root.selectedId !== "" && root.panel.loading === true
            anchors.centerIn: parent
            size: 32
        }
        // 3 — error.
        Text {
            visible: root.selectedId !== "" && root.panel.loading !== true
                && (root.panel.error || "") !== ""
            anchors.centerIn: parent
            text: QbzSession.tr("Couldn't load albums.", QbzSession.trRev)
            color: theme.textPrimary
            font.pixelSize: theme.fontBody
            font.weight: theme.weightSemibold
        }
        // 4 — nothing to show.
        Text {
            visible: root.selectedId !== "" && root.panel.loading !== true
                && (root.panel.error || "") === "" && (root.panel.total || 0) === 0
            anchors.centerIn: parent
            text: QbzSession.tr("No albums found.", QbzSession.trRev)
            color: theme.textMuted
            font.pixelSize: theme.fontBody
        }
        // 5 — the sections.
        Flickable {
            id: paneFlick
            visible: root.selectedId !== "" && root.panel.loading !== true
                && (root.panel.error || "") === "" && (root.panel.total || 0) > 0
            anchors.fill: parent
            clip: true
            contentWidth: width
            contentHeight: paneCol.height
            boundsBehavior: Flickable.StopAtBounds

            Column {
                id: paneCol
                x: 20
                width: paneFlick.width - 32
                spacing: 18

                Repeater {
                    model: root.panel.sections || []
                    delegate: Column {
                        id: sec
                        required property var modelData
                        width: paneCol.width
                        spacing: 8
                        Text {
                            text: sec.modelData.title || ""
                            color: theme.textPrimary
                            font.pixelSize: theme.fontBody
                            font.weight: theme.weightSemibold
                        }
                        // Explicit grid, not AlbumCollection: that component
                        // reads a baked `artPath` off each row, and these rows
                        // carry `artUrl` only (the covers arrive url-keyed —
                        // see coverMap). Same column math, same 200x246 card.
                        Item {
                            id: grid
                            width: parent.width
                            readonly property int cardW: 200
                            readonly property int cardH: 246
                            readonly property int gap: 24
                            readonly property var albums: sec.modelData.albums || []
                            readonly property int columns: Math.max(
                                1, Math.floor((grid.width + grid.gap) / (grid.cardW + grid.gap)))
                            readonly property int rowCount: Math.ceil(grid.albums.length / grid.columns)
                            height: grid.rowCount > 0
                                ? grid.rowCount * grid.cardH + (grid.rowCount - 1) * grid.gap
                                : 0
                            Repeater {
                                model: grid.albums
                                delegate: Item {
                                    id: gcell
                                    required property var modelData
                                    required property int index
                                    x: (gcell.index % grid.columns) * (grid.cardW + grid.gap)
                                    y: Math.floor(gcell.index / grid.columns) * (grid.cardH + grid.gap)
                                    width: grid.cardW
                                    height: grid.cardH
                                    AlbumCard {
                                        albumId: gcell.modelData.id
                                        title: gcell.modelData.title
                                        artist: gcell.modelData.artist
                                        artistId: gcell.modelData.artistId
                                        genre: gcell.modelData.genre
                                        year: gcell.modelData.year
                                        qualityTier: gcell.modelData.qualityTier
                                        qualityDetail: gcell.modelData.qualityDetail || ""
                                        artSource: root.coverMap[gcell.modelData.artUrl] || ""
                                        // The pin payload persists the REMOTE
                                        // url, never the file:// cache path.
                                        artworkUrl: gcell.modelData.artUrl || ""
                                        isPinned: gcell.modelData.isPinned === true
                                        isFavorite: gcell.modelData.isFavorite === true
                                    }
                                }
                            }
                        }
                    }
                }
                // Footer — the full artist page (compilations and
                // other-artist content live there).
                Item {
                    width: parent.width
                    height: 40
                    Text {
                        id: footerText
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Go to artist page", QbzSession.trRev)
                        color: theme.accent
                        font.pixelSize: theme.fontBody
                    }
                    MouseArea {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        width: footerText.implicitWidth
                        height: 24
                        cursorShape: Qt.PointingHandCursor
                        onClicked: QbzArtist.openArtist(root.selectedId)
                    }
                }
                Item { width: 1; height: 60 }
            }
        }
        QbzScrollBar {
            anchors.right: parent.right
            anchors.rightMargin: 4
            anchors.top: parent.top
            anchors.bottom: parent.bottom
            target: paneFlick
            visible: paneFlick.visible && paneFlick.contentHeight > paneFlick.height
        }
    }
}
