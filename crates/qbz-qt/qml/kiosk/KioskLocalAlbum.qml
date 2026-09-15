// Local album Kiosk host: one scrolling header and a real delegate window.
// All commands use the same local album document and actions as Full UI.
import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../rows"
import "../theme"
import "../views/local"

Rectangle {
    id: root
    color: theme.surfaceMain
    QbzTheme { id: theme }
    readonly property var doc: {
        try { return JSON.parse(QbzLocal.localAlbumJson) || ({}) } catch(e) { return ({}) }
    }
    readonly property var album: doc.album || null
    readonly property var tracks: doc.tracks || []
    property string trackQuery: ""
    property var selected: ({})
    property bool multiSelect: false
    readonly property bool multiSelectOn: multiSelect
    readonly property int selectedCount: Object.keys(selected).length
    SelectionModel { id: selection }
    readonly property var visibleTracks: {
        var query = trackQuery.trim().toLowerCase()
        return query === "" ? tracks : tracks.filter(function (t) {
            return (t.title || "").toLowerCase().indexOf(query) >= 0
        })
    }
    readonly property var allArtists: album ? (album.allArtists || album.artist || "").split(",") : []
    property string coverPath: ""
    onAlbumChanged: {
        coverPath = ""
        if (album && album.artKey) QbzLocal.artworkWindow(JSON.stringify([album.artKey]))
    }
    Connections {
        target: QbzLocal
        function onLocalArtworkReady(key, path) {
            if (root.album && key === root.album.artKey) root.coverPath = path
        }
    }
    function selectedIds() { return tracks.filter(function(t) { return selected[t.id] === true }).map(function(t) { return t.id }) }
    function bulkAction(action) {
        if (action === "clear") { selected = ({}); return }
        if (action === "select-all") {
            var next = {}; visibleTracks.forEach(function(t) { next[t.id] = true }); selected = next; return
        }
        QbzLocal.bulkAction("track", JSON.stringify(selectedIds()), action)
    }
    function selectAll() { multiSelect = true; bulkAction("select-all") }
    function exitMultiSelectMode() { multiSelect = false; selected = ({}) }
    function toggleSelected(id, mods) { selected = selection.next(selected, id, visibleTracks, mods === undefined ? Qt.NoModifier : mods) }
    readonly property string navigationStateJson: JSON.stringify({trackQuery: trackQuery})
    function restoreNavigationState(state) { trackQuery = state.trackQuery || "" }
    // An in-place album switch starts at the top; the first document of a
    // mount does not touch the viewport (ScrollMemory may be restoring it).
    property string loadedAlbumId: ""
    onDocChanged: {
        exitMultiSelectMode()
        var id = root.album ? String(root.album.id || "") : ""
        if (id === root.loadedAlbumId)
            return
        if (root.loadedAlbumId !== "")
            rowsModel.scrollToTop()
        root.loadedAlbumId = id
    }

    ListView {
        id: list
        anchors.fill: parent
        anchors.margins: 12
        clip: true
        // Swapped on a QbzArrayModel, never on `model:` — a fresh array there
        // makes Qt 6.11 focus delegate 0 and the header's search box loses the
        // keyboard on every keystroke (see the control's header).
        model: QbzArrayModel { id: rowsModel; view: list; rows: root.visibleTracks; delegate: trackDelegate }
        cacheBuffer: height
        reuseItems: true
        boundsBehavior: Flickable.StopAtBounds
        header: Column {
            width: list.width
            spacing: 12
            LocalAlbumHeader {
                width: parent.width
                kioskHost: true
                visible: !!root.album
                album: root.album
                allArtists: root.allArtists
                versions: root.album ? root.album.versions || [] : []
                coverSource: root.coverPath
                infoLine: root.album ? [root.album.year || "", root.tracks.length + " " + QbzSession.tr("tracks", QbzSession.trRev), root.album.duration || ""].filter(function(t) { return t !== "" }).join(" • ") : ""
                onOpenArtist: function(name) { QbzLocal.openArtistByName(name); QbzShell.navigateTo("local") }
            }
            Row {
                spacing: 8
                width: parent.width
                QbzLineEdit {
                    kioskHost: true
                    width: Math.max(120, parent.width - selectButton.width - 8)
                    searchMode: true
                    text: root.trackQuery
                    onEdited: function(value) { root.trackQuery = value }
                }
                SettingsButton {
                    id: selectButton
                    kioskHost: true
                    text: QbzSession.tr("Select", QbzSession.trRev)
                    minWidth: 80
                    onClicked: root.multiSelect ? root.exitMultiSelectMode() : root.multiSelect = true
                }
            }
            Row {
                visible: root.multiSelect
                width: parent.width
                spacing: 8
                SettingsButton { kioskHost: true; text: QbzSession.tr("Select all", QbzSession.trRev); minWidth: 100; onClicked: root.selectAll() }
                SettingsButton { kioskHost: true; text: QbzSession.tr("Clear", QbzSession.trRev); minWidth: 80; onClicked: root.bulkAction("clear") }
                SettingsButton {
                    id: selectedMenuButton
                    kioskHost: true; text: QbzSession.tr("More options", QbzSession.trRev); minWidth: 100
                    onClicked: selectedMenu.openBelowRight(selectedMenuButton)
                }
            }
            KioskSkeleton {
                width:parent.width; height:visible ? 180 : 0
                visible:QbzLocal.localAlbumLoading || root.visibleTracks.length===0 || (root.doc.error || "")!==""
                kind:"list"; loading:QbzLocal.localAlbumLoading; empty:root.visibleTracks.length===0
                error:root.doc.error || ""
            }
        }
        // Handed to rowsModel: Qt 6.8/6.9 ignore a view's delegate when the
        // model is an external DelegateModel, and rendered zero rows.
        Component {
            id: trackDelegate
            Column {
                id: row
                required property var modelData
                required property int index
                width: list.width
                readonly property int disc: modelData.disc || 1
                readonly property bool startsDisc: index === 0 || (root.visibleTracks[index-1].disc || 1) !== disc
                Item {
                    width: parent.width
                    height: row.startsDisc ? 44 : 0
                    visible: row.startsDisc
                    Text { anchors.left: parent.left; anchors.verticalCenter: parent.verticalCenter; text: QbzSession.tr("Disc", QbzSession.trRev) + " " + row.disc; color: theme.textSecondary; font.pixelSize: 16 }
                    SettingsButton {
                        id: discButton
                        kioskHost: true; anchors.right: parent.right
                        iconName: "ellipsis"; minWidth: 44
                        onClicked: discMenu.openBelowRight(discButton)
                    }
                    CardMenu {
                        id: discMenu; kioskHost: true
                        entries: [
                            {label: QbzSession.tr("Play", QbzSession.trRev), action:"play"},
                            {label: QbzSession.tr("Play next", QbzSession.trRev), action:"next"},
                            {label: QbzSession.tr("Play later", QbzSession.trRev), action:"later"},
                            {label: QbzSession.tr("Add to queue", QbzSession.trRev), action:"queue"}
                        ]
                        onPicked: function(a) { QbzLocal.albumDiscAction(row.disc, a) }
                    }
                }
                LocalTrackRow {
                    width: parent.width; kioskHost: true
                    item: row.modelData
                    number: row.modelData.number > 0 ? row.modelData.number : row.index + 1
                    showAlbum: false; showArtwork: false; zebra: true
                    selectMode: root.multiSelect
                    checked: root.selected[row.modelData.id] === true
                    onPlayRequested: QbzLocal.albumSelectedAction("play", row.modelData.id)
                    onEnqueueRequested: function(mode) { QbzLocal.enqueue("track", row.modelData.id, mode) }
                    onToggleSelect: function(mods) { root.toggleSelected(row.modelData.id, mods) }
                }
            }
        }
    }
    CardMenu {
        id: selectedMenu; kioskHost: true; menuWidth: 280
        entries: [
            {label: QbzSession.tr("Play next", QbzSession.trRev), action:"play-next"},
            {label: QbzSession.tr("Play later", QbzSession.trRev), action:"play-later"},
            {label: QbzSession.tr("Add to queue", QbzSession.trRev), action:"queue"},
            {label: QbzSession.tr("Add to playlist", QbzSession.trRev), action:"add-to-playlist"},
            {label: QbzSession.tr("Add to Mixtape/Collection", QbzSession.trRev), action:"add-to-mixtape"}
        ]
        onPicked: function(a) { if (root.selectedCount > 0) root.bulkAction(a) }
    }
    ScrollMemory { target: list; scope: "localalbum"; relativeToOrigin: true }
    QbzScrollBar { target: list; anchors.right: parent.right; anchors.top: parent.top; anchors.bottom: parent.bottom }
}
