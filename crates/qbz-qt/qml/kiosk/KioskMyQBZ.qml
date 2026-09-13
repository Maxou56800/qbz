// Kiosk projection of the existing documents. Only viewport cards own images.
import QtQuick
import QtQuick.Controls
import QtQuick.Window
import com.blitzfc.qbz
import "../theme"
import "../controls"

Rectangle {
    id: root
    property string kind: "mixtape"
    readonly property bool collectionsTab: kind === "collection"
    readonly property bool compactHeader: height < 360
    readonly property string route: collectionsTab ? "collections" : "mixtapes"
    property var doc: ({loading: true})
    property var cards: []
    property int revision: 0
    property string signature: ""
    property string parseError: ""
    property string lastArtWindow: ""
    color: "transparent"
    QbzTheme { id: theme }
    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }
    function refresh() {
        try {
            var d = JSON.parse(root.collectionsTab ? QbzMyQbz.collectionsJson : QbzMyQbz.mixtapesJson)
            root.parseError = ""
            var fresh = d.cards || []
            var sig = root.route + ":" + fresh.map(function(c) { return c.id }).join("|")
            if (sig === root.signature) {
                for (var i = 0; i < fresh.length; ++i) Object.assign(root.cards[i], fresh[i])
            } else {
                root.signature = sig
                root.cards = fresh
                root.lastArtWindow = ""
            }
            root.doc = d
            root.revision++
            Qt.callLater(root.requestArtwork)
            root.publishNav()
        } catch (e) { root.parseError = root.t("Error") }
    }
    function publishNav() { QbzKioskNav.publishNav(2, grid.columns, 2 + cards.length, false) }
    function requestArtwork() {
        if (root.doc.loading || !root.cards.length || grid.height <= 0) return
        var first = Math.max(0, Math.floor(grid.contentY / grid.cellHeight) * grid.columns)
        var last = Math.min(root.cards.length - 1, (Math.ceil((grid.contentY + grid.height) / grid.cellHeight) + 1) * grid.columns - 1)
        var px = Math.ceil((grid.cellWidth - 16) * Screen.devicePixelRatio)
        var key = root.signature + ":" + first + ":" + last + ":" + px + ":" + root.cards.slice(first, last + 1).map(function(c) { return (c.cellUrls || [])[0] || "" }).join("|")
        if (key === root.lastArtWindow) return
        root.lastArtWindow = key
        QbzMyQbz.kioskGridArtwork(root.route, first, last, px)
    }
    onCollectionsTabChanged: refresh()
    Component.onCompleted: refresh()
    Connections {
        target: QbzMyQbz
        function onCollectionsJsonChanged() { if (root.collectionsTab) root.refresh() }
        function onMixtapesJsonChanged() { if (!root.collectionsTab) root.refresh() }
    }
    KioskNavigation { route: root.route; snapshot: ({}) }
    ScrollMemory { id: scrollMemory; target: grid; scope: root.route; onScopeChanged: Qt.callLater(scrollMemory._begin) }
    Connections {
        target: QbzKioskNav
        function onIndexChanged() {
            if (QbzKioskNav.zone === "content" && QbzKioskNav.index >= 2)
                grid.positionViewAtIndex(QbzKioskNav.index - 2, GridView.Contain)
        }
        function onActivateSeqChanged() {
            if (!QbzKioskNav.navActive || QbzKioskNav.zone !== "content") return
            var i = QbzKioskNav.index
            if (i < 2) QbzShell.navigateTo(i === 0 ? "mixtapes" : "collections")
            else if (i - 2 < root.cards.length) QbzMyQbz.openCard(root.cards[i - 2].id)
        }
    }
    Row {
        id: tabs
        x: 16
        spacing: 12
        height: 64
        KioskTab { text: root.t("Mixtapes"); selected: !root.collectionsTab; onClicked: QbzShell.navigateTo("mixtapes") }
        KioskTab { text: root.t("Collections"); selected: root.collectionsTab; onClicked: QbzShell.navigateTo("collections") }
        KioskTab { text: "+"; onClicked: QbzMyQbz.createOpen(root.kind) }
    }
    TextField {
        QbzTextEditMenu { }
        id: search
        anchors.top: root.compactHeader ? root.top : tabs.bottom
        anchors.left: root.compactHeader ? tabs.right : parent.left
        anchors.right: parent.right
        anchors.margins: 16
        anchors.topMargin: root.compactHeader ? 8 : 16
        height: 48
        font.pixelSize: 18
        color: theme.textPrimary
        placeholderTextColor: theme.textMuted
        background: Rectangle { color: theme.surfaceElevated; border.color: theme.borderSubtle; radius: 4 }
        placeholderText: root.t("Search")
        text: root.doc.search || ""
        onTextEdited: QbzMyQbz.gridSearch(root.route, text)
    }
    GridView {
        id: grid
        objectName: "kioskMyQbzGrid"
        anchors.top: search.bottom
        anchors.bottom: parent.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.margins: 16
        clip: true
        readonly property int columns: Math.max(2, Math.floor(width / (root.compactHeader ? 130 : 160)))
        cellWidth: width / columns
        cellHeight: cellWidth + 44
        cacheBuffer: cellHeight
        boundsBehavior: Flickable.StopAtBounds
        model: root.cards
        visible: !root.doc.loading && !root.parseError && root.cards.length > 0
        onContentYChanged: Qt.callLater(root.requestArtwork)
        onHeightChanged: Qt.callLater(root.requestArtwork)
        onColumnsChanged: { root.publishNav(); Qt.callLater(root.requestArtwork) }
        delegate: Rectangle {
            id: card
            required property int index
            required property var modelData
            readonly property var row: root.revision >= 0 ? Object.assign({}, modelData) : ({})
            width: grid.cellWidth - 16
            height: grid.cellHeight - 16
            color: "transparent"
            border.width: QbzKioskNav.navActive && QbzKioskNav.zone === "content" && QbzKioskNav.index === index + 2 ? 2 : 0
            border.color: theme.accent
            Rectangle { width: parent.width; height: width; radius: theme.radiusMd; color: theme.surfaceElevated }
            KioskArtwork {
                width: parent.width
                height: width
                source: card.row.hasCustomCover ? (card.row.customCoverPath || "") : ((card.row.cellPaths || [])[0] || "")
                radius: theme.radiusMd
            }
            Text { y: parent.width + 6; width: parent.width; text: card.row.name || ""; color: theme.textPrimary; font.pixelSize: 16; elide: Text.ElideRight }
            Text { y: parent.width + 28; width: parent.width; text: card.row.meta || ""; color: theme.textMuted; font.pixelSize: 13; elide: Text.ElideRight }
            MouseArea { anchors.fill: parent; onClicked: QbzMyQbz.openCard(card.row.id) }
        }
    }
    KioskSkeleton {
        anchors.fill: grid
        loading: root.doc.loading === true
        error: root.parseError || root.doc.error || ""
        empty: !loading && root.cards.length === 0
        emptyText: root.t("No results")
    }
    component KioskTab: Rectangle {
        id: button
        property string text: ""
        property bool selected: false
        signal clicked()
        QbzTheme { id: buttonTheme }
        width: Math.max(64, label.implicitWidth + 28)
        height: 64
        color: selected ? buttonTheme.surfaceElevated : "transparent"
        Text { id: label; anchors.centerIn: parent; text: button.text; font.pixelSize: 18; color: buttonTheme.textPrimary }
        MouseArea { anchors.fill: parent; onClicked: button.clicked() }
    }
}
