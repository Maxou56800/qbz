// Local Library previews shared by the All and category search tabs.
// Result actions carry the page snapshot revision, never a Qobuz/native DB ID.
pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    id: root
    property var sections: []
    property string revision: ""
    property int tab: 0
    readonly property bool hasResults: sections.some(function(section) { return root.accepts(section.kind) })
    signal activated(string revision, int index, string action)
    signal viewMore(string revision, string kind)
    spacing: 24

    QbzTheme { id: theme }

    function accepts(kind) {
        return tab === 0 || (tab === 1 && kind === "local-album")
            || (tab === 2 && kind === "local") || (tab === 3 && kind === "local-artist")
    }
    function titleFor(kind) {
        if (kind === "local-album") return QbzSession.tr("Albums on Local Library", QbzSession.trRev)
        if (kind === "local-artist") return QbzSession.tr("Artists on Local Library", QbzSession.trRev)
        return QbzSession.tr("On Local Library", QbzSession.trRev)
    }
    function menuFor(row) {
        var t = QbzSession.tr
        var r = QbzSession.trRev
        if (row.kind === "artist")
            return [{label:t("Open artist", r), icon:"user", action:"open"}]
        var entries = []
        if (row.kind === "album")
            entries.push({label:t("Open album", r), icon:"library-big", action:"open"})
        entries.push({label:t("Play", r), icon:"play-fill", action:"play"})
        entries.push({label:t("Play next", r), icon:"list-start", action:"next"})
        entries.push({label:t("Play later", r), icon:"list-plus", action:"later"})
        entries.push({label:t("Add to queue", r), icon:"list-end", action:"queue"})
        if (row.kind === "track") {
            entries.push({label:t("Add to playlist", r), icon:"list-music", action:"add-to-playlist"})
            if ((row.albumId || "") !== "")
                entries.push({label:t("Open containing album", r), icon:"disc-3", action:"open-album"})
        }
        return entries
    }

    Repeater {
        model: root.sections
        delegate: Column {
            id: section
            required property var modelData
            objectName: "localSearchSection-" + section.modelData.kind
            visible: root.accepts(section.modelData.kind)
            width: root.width
            spacing: 8
            QbzSectionHeader {
                title: root.titleFor(section.modelData.kind)
                showChevrons: false
                showViewAll: section.modelData.hasMore === true
                viewAllLabel: QbzSession.tr("View more", QbzSession.trRev)
                onViewAllClicked: root.viewMore(root.revision, section.modelData.kind)
            }
            Repeater {
                model: section.visible ? section.modelData.rows : []
                delegate: Rectangle {
                    id: resultRow
                    required property var modelData
                    objectName: "localSearchRow-" + resultRow.modelData.flatIndex
                    width: section.width
                    height: 68
                    radius: theme.radiusSm
                    color: hit.containsMouse ? theme.surfaceHover : "transparent"
                    RoundedImage {
                        x: 8; width: 48; height: 48
                        anchors.verticalCenter: parent.verticalCenter
                        source: resultRow.modelData.artPath || ""
                        radius: resultRow.modelData.kind === "artist" ? 24 : 5
                    }
                    Column {
                        anchors.left: parent.left; anchors.leftMargin: 70
                        anchors.right: menuButton.left; anchors.rightMargin: 8
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 2
                        Text {
                            width: parent.width
                            text: resultRow.modelData.title || ""
                            color: theme.textPrimary; font.pixelSize: 14
                            elide: Text.ElideRight
                        }
                        Text {
                            width: parent.width
                            text: resultRow.modelData.subtitle || ""
                            color: theme.textMuted; font.pixelSize: 12
                            elide: Text.ElideRight
                        }
                        Text {
                            width: parent.width
                            visible: text !== ""
                            text: resultRow.modelData.qualityDetail || ""
                            color: theme.textSecondary; font.pixelSize: 10
                            elide: Text.ElideRight
                        }
                    }
                    MouseArea {
                        id: hit
                        objectName: "localSearchHit-" + resultRow.modelData.flatIndex
                        anchors.fill: parent
                        anchors.rightMargin: 44
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        acceptedButtons: Qt.LeftButton | Qt.RightButton
                        onClicked: function(mouse) {
                            if (mouse.button === Qt.RightButton) {
                                menuLoader.active = true
                                var menu = menuLoader.item as CardMenu
                                if (menu) menu.openAtCursor(hit, mouse.x, mouse.y)
                            } else {
                                root.activated(root.revision, resultRow.modelData.flatIndex,
                                    resultRow.modelData.kind === "track" ? "play" : "open")
                            }
                        }
                    }
                    QbzIconButton {
                        id: menuButton
                        objectName: "localSearchMenu-" + resultRow.modelData.flatIndex
                        anchors.right: parent.right; anchors.rightMargin: 8
                        anchors.verticalCenter: parent.verticalCenter
                        name: "ellipsis"; btnSize: 28; iconSize: 14
                        onClicked: {
                            menuLoader.active = true
                            var menu = menuLoader.item as CardMenu
                            if (menu) menu.openBelowRight(menuButton)
                        }
                        HoverHandler { id: menuHover }
                        ToolTip.visible: menuHover.hovered
                        ToolTip.text: QbzSession.tr("More options", QbzSession.trRev)
                        ToolTip.delay: 350
                    }
                    Loader {
                        id: menuLoader
                        objectName: "localSearchMenuLoader-" + resultRow.modelData.flatIndex
                        active: false
                        sourceComponent: CardMenu {
                            objectName: "localSearchMenuPopup-" + resultRow.modelData.flatIndex
                            entries: root.menuFor(resultRow.modelData)
                            onPicked: function(action) {
                                root.activated(root.revision, resultRow.modelData.flatIndex, action)
                            }
                        }
                    }
                }
            }
        }
    }
}
