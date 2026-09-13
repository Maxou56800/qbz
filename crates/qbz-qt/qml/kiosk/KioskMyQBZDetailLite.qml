// Complete collection rows in a touch list; desktop keeps its own presentation.
import QtQuick
import QtQuick.Controls
import QtQuick.Window
import com.blitzfc.qbz
import "../theme"
import "../controls"

Rectangle {
    id: root
    color: "transparent"
    property var doc: ({loading: true})
    property var rows: []
    property int revision: 0
    property string signature: ""
    property string parseError: ""
    property string lastArtWindow: ""
    property var menuEntries: []
    property var menuItem: ({})
    property bool menuOpen: false
    readonly property bool compactHeader: height < 360
    QbzTheme { id: theme }
    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }
    function refresh() {
        try {
            var d = JSON.parse(QbzMyQbz.detailJson)
            var fresh = d.items || []
            var sig = (d.id || "") + ":" + fresh.map(function(r) { return r.position + ":" + r.sourceItemId }).join("|")
            if (sig === root.signature) {
                for (var i = 0; i < fresh.length; ++i) Object.assign(root.rows[i], fresh[i])
            } else {
                root.signature = sig
                root.rows = fresh
                root.lastArtWindow = ""
            }
            root.doc = d
            root.parseError = ""
            root.revision++
            root.publishNav()
            Qt.callLater(root.requestArtwork)
        } catch (e) { root.parseError = root.t("Error") }
    }
    function patch(json) {
        try {
            var p = JSON.parse(json)
            if (p.id !== root.doc.id) return
            var byPosition = ({})
            root.rows.forEach(function(r) { byPosition[r.position] = r })
            ;(p.rows || []).forEach(function(r) { if (byPosition[r.position]) Object.assign(byPosition[r.position], r) })
            root.revision++
            Qt.callLater(root.requestArtwork)
        } catch (e) { console.warn("Kiosk MyQBZ row patch:", e) }
    }
    function publishNav() { QbzKioskNav.publishNav(0, 1, rows.length, false) }
    function requestArtwork() {
        if (root.doc.loading || !root.rows.length || list.height <= 0) return
        var first = Math.max(0, Math.floor(list.contentY / 72))
        var last = Math.min(root.rows.length - 1, Math.ceil((list.contentY + list.height) / 72))
        var px = Math.ceil(48 * Screen.devicePixelRatio)
        var key = root.signature + ":" + first + ":" + last + ":" + px + ":" + root.rows.slice(first, last + 1).map(function(r) { return r.artUrl || "" }).join("|")
        if (key === root.lastArtWindow) return
        root.lastArtWindow = key
        QbzMyQbz.kioskDetailArtwork(first, last, px)
    }
    function openRow(r) {
        if (root.doc.selectMode) QbzMyQbz.detailToggleItemSelect(r.position)
        else QbzMyQbz.openItem(r.source, r.itemType, r.sourceItemId)
    }
    function entry(label, action) { return {label: root.t(label), action: action} }
    function showRowMenu(r) {
        root.menuItem = r
        root.menuEntries = [entry("Play", "row:play"), entry("Play next", "row:play-next"), entry("Play later", "row:play-later"), entry("Add to queue", "row:add-to-queue"), entry("Remove from collection", "row:remove")]
        if (r.subtitleIsLink) root.menuEntries.push(entry("Go to artist", "artist"))
        root.menuOpen = true
    }
    function showMenu() {
        var m = [entry("Shuffle", "shuffle"), entry("Random queue", "dj-mix"), entry("Rename", "rename"), entry("Edit description", "description"), entry("Upload cover", "upload-cover")]
        if (root.doc.hasCustomCover) m.push(entry("Clear custom cover", "clear-cover"))
        m.push(entry((root.doc.playMode || "in_order") === "in_order" ? "Album shuffle" : "In order", "play-mode"))
        if (root.doc.kind !== "artist_collection") m.push(entry(root.doc.kind === "mixtape" ? "Convert to Collection" : "Convert to Mixtape", "convert"))
        m.push(entry("Select", "select"))
        m.push(entry("Reset", "reset"))
        ;[["Position","position"],["Name","name"],["Year","year"],["Tracks","tracks"]].forEach(function(s) { m.push(entry(s[0], "sort:" + s[1])) })
        ;[["All","all"],["Albums","album"],["Tracks","track"],["Playlists","playlist"]].forEach(function(s) { m.push(entry(s[0], "type:" + s[1])) })
        ;[["Qobuz","qobuz"],["Plex","plex"],["Local","local"]].forEach(function(s) { m.push(entry(s[0], "source:" + s[1])) })
        if (root.doc.selectedCount > 0) {
            ;[["Add to queue","add-to-queue"],["Play next","play-next"],["Play later","play-later"],["Add to playlist","add-to-playlist"],["Add to Mixtape/Collection","add-to-mixtape"],["Remove","remove-selected"],["Clear","clear"]].forEach(function(s) { m.push(entry(s[0], "bulk:" + s[1])) })
        }
        m.push(entry("Delete", "delete"))
        root.menuEntries = m
        root.menuOpen = true
    }
    function action(a) {
        root.menuOpen = false
        if (a.indexOf("row:") === 0) {
            var op = a.slice(4)
            if (op === "play") QbzMyQbz.playItem(root.menuItem.sourceItemId)
            else if (op === "remove") QbzMyQbz.removeItem(root.menuItem.position)
            else QbzMyQbz.itemAction(root.menuItem.sourceItemId, op)
        } else if (a.indexOf("sort:") === 0) QbzMyQbz.detailSetSort(a.slice(5))
        else if (a.indexOf("type:") === 0) QbzMyQbz.detailSetTypeFilter(a.slice(5))
        else if (a.indexOf("source:") === 0) QbzMyQbz.detailToggleSourceFilter(a.slice(7))
        else if (a.indexOf("bulk:") === 0) QbzMyQbz.bulkAction(a.slice(5))
        else if (a === "artist") QbzMyQbz.openArtist(root.menuItem.source, root.menuItem.subtitle, root.menuItem.artistId)
        else if (a === "shuffle") QbzMyQbz.shuffle()
        else if (a === "dj-mix") QbzMyQbz.djMix()
        else if (a === "rename") QbzMyQbz.openRename()
        else if (a === "description") QbzMyQbz.openDescription()
        else if (a === "upload-cover") QbzMyQbz.uploadCover()
        else if (a === "clear-cover") QbzMyQbz.removeCover()
        else if (a === "play-mode") QbzMyQbz.togglePlayMode()
        else if (a === "convert") QbzMyQbz.convertKind()
        else if (a === "delete") QbzMyQbz.openDelete()
        else if (a === "select") QbzMyQbz.detailToggleSelectMode()
        else if (a === "reset") QbzMyQbz.detailResetFilters()
    }
    Component.onCompleted: { refresh(); QbzMyQbz.detailResync() }
    Connections {
        target: QbzMyQbz
        function onDetailJsonChanged() { root.refresh() }
        function onDetailRowsPatched(json) { root.patch(json) }
    }
    Connections {
        target: QbzKioskNav
        function onIndexChanged() { if (QbzKioskNav.zone === "content") list.positionViewAtIndex(QbzKioskNav.index, ListView.Contain) }
        function onActivateSeqChanged() {
            if (QbzKioskNav.navActive && QbzKioskNav.zone === "content" && !root.menuOpen && root.rows[QbzKioskNav.index]) root.openRow(root.rows[QbzKioskNav.index])
        }
    }
    KioskNavigation { route: "mixtapedetail"; snapshot: ({}) }
    ScrollMemory { target: list; scope: "mixtapedetail" }
    Item {
        id: header
        x: 16
        width: parent.width - 32
        height: root.compactHeader ? 120 : 180
        Column {
            width: root.compactHeader ? Math.max(0, header.width - headerActions.width - 12) : parent.width
            y: root.compactHeader ? Math.max(0, (64 - height) / 2) : 0
            spacing: 3
            Text { width: parent.width; text: root.doc.name || ""; color: theme.textPrimary; font.pixelSize: root.compactHeader ? 21 : 24; font.weight: Font.DemiBold; elide: Text.ElideRight }
            Text { width: parent.width; text: root.doc.meta || ""; color: theme.textMuted; font.pixelSize: 14; elide: Text.ElideRight }
        }
        Row {
            id: headerActions
            x: root.compactHeader ? header.width - width : 0
            y: root.compactHeader ? 0 : 52
            enabled: root.doc.found === true && !root.doc.loading
            opacity: enabled ? 1 : 0.5
            spacing: 8
            TouchButton { text: root.t("Play"); onClicked: QbzMyQbz.playAll() }
            TouchButton { visible: !root.compactHeader; text: root.t("Shuffle"); onClicked: QbzMyQbz.shuffle() }
            TouchButton { visible: !root.compactHeader; text: root.t("Random queue"); onClicked: QbzMyQbz.djMix() }
            TouchButton { text: "⋯" + (root.doc.selectedCount > 0 ? " " + root.doc.selectedCount : ""); onClicked: root.showMenu() }
        }
        TextField {
            QbzTextEditMenu { }
            y: root.compactHeader ? 70 : 124
            width: parent.width
            height: 48
            font.pixelSize: 18
            color: theme.textPrimary
            placeholderTextColor: theme.textMuted
            background: Rectangle { color: theme.surfaceElevated; border.color: theme.borderSubtle; radius: 4 }
            placeholderText: root.t("Search")
            text: root.doc.search || ""
            onTextEdited: QbzMyQbz.detailSearch(text)
        }
    }
    ListView {
        id: list
        objectName: "kioskMyQbzDetailList"
        anchors.top: header.bottom
        anchors.bottom: parent.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.margins: 12
        clip: true
        model: root.rows
        cacheBuffer: 72
        boundsBehavior: Flickable.StopAtBounds
        visible: !root.doc.loading && root.doc.found === true && !root.parseError
        onContentYChanged: Qt.callLater(root.requestArtwork)
        onHeightChanged: Qt.callLater(root.requestArtwork)
        delegate: Rectangle {
            id: row
            required property int index
            required property var modelData
            readonly property var item: root.revision >= 0 ? Object.assign({}, modelData) : ({})
            width: list.width
            height: 72
            color: item.selected ? theme.surfaceElevated : "transparent"
            border.width: QbzKioskNav.navActive && QbzKioskNav.zone === "content" && QbzKioskNav.index === index ? 2 : 0
            border.color: theme.accent
            KioskArtwork { x: 8; y: 12; width: 48; height: 48; source: row.item.artPath || ""; radius: 4 }
            Column {
                x: 68; width: Math.max(0, parent.width - 136); anchors.verticalCenter: parent.verticalCenter; spacing: 3
                Text { width: parent.width; text: row.item.title || ""; color: theme.textPrimary; font.pixelSize: 18; elide: Text.ElideRight }
                Text { width: parent.width; text: [row.item.subtitle || "", row.item.qualityDetail || row.item.typeLabel || "", row.item.yearText || ""].filter(function(s) { return s !== "" }).join(" · "); color: theme.textMuted; font.pixelSize: 13; elide: Text.ElideRight }
            }
            MouseArea { anchors.fill: parent; anchors.rightMargin: 64; onClicked: root.openRow(row.item); onPressAndHold: root.showRowMenu(row.item) }
            TouchButton { anchors.right: parent.right; anchors.verticalCenter: parent.verticalCenter; text: "⋯"; onClicked: root.showRowMenu(row.item) }
        }
    }
    KioskSkeleton {
        anchors.fill: list
        kind: "list"
        loading: root.doc.loading === true
        error: root.parseError || root.doc.error || (!loading && root.doc.found === false ? root.t("Not found") : "")
        empty: !loading && root.doc.found === true && root.rows.length === 0
        emptyText: root.t("No items yet. Add albums, tracks, or playlists from their detail pages.")
    }
    // Touch actions occupy the content panel, with a virtual list and a fixed exit.
    Rectangle {
        anchors.fill: parent
        z: 20
        visible: root.menuOpen
        color: theme.surfaceMain
        MouseArea { anchors.fill: parent }
        TouchButton { id: closeMenu; anchors.top: parent.top; anchors.right: parent.right; text: root.t("Close"); onClicked: root.menuOpen = false }
        ListView {
            anchors.top: closeMenu.bottom; anchors.bottom: parent.bottom; width: parent.width; clip: true
            model: root.menuOpen ? root.menuEntries : []
            delegate: TouchButton { required property var modelData; width: ListView.view.width; text: modelData.label; onClicked: root.action(modelData.action) }
        }
    }
    component TouchButton: Rectangle {
        id: button
        property string text: ""
        signal clicked()
        QbzTheme { id: buttonTheme }
        width: Math.max(64, label.implicitWidth + 24)
        height: 64
        color: buttonTheme.surfaceElevated
        radius: 4
        Text { id: label; anchors.centerIn: parent; text: button.text; font.pixelSize: 16; color: buttonTheme.textPrimary }
        MouseArea { anchors.fill: parent; onClicked: button.clicked() }
    }
}
