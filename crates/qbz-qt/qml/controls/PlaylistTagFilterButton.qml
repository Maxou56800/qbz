// "Filter by category" trigger for the Home rail of Qobuz Playlists — port of
// `crates/qbz-ui/ui/discover/PlaylistTagFilter.slint:66-118`.
//
// 220x30, radius 6, NO outline: the reference states the size contract
// explicitly ("sm variant — matches QbzSelect { sm: true } used by the toolbar
// selects: 30px tall, 6px radius, no outline"), so this is the small toolbar
// control, not the 34px browse-header one that BrowseGenreButton draws.
//
// Extracted rather than inlined into HomeView for the reason BrowseGenreButton
// states about itself: one control, drawn identically wherever it is mounted.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Rectangle {
    id: root

    property int count: 0
    signal clicked()

    // The selected tag NAMES, for the applied-filters tooltip. The host has
    // them (it holds the tag catalogue); this control only knows the count.
    property var selectedNames: []

    QbzTheme { id: theme }

    width: 220
    height: 30
    radius: 6
    // Translucent at rest under the dynamic background, like every other chip
    // in this toolbar band (PlaylistTagFilter.slint:77).
    color: tagArea.containsMouse
        ? theme.elevatedHoverFill
        : theme.elevatedFill

    QbzFilterTip {
        id: tagTip
        ownerKey: "home-playlist-tags"
        anchor: root
        groups: root.selectedNames.length > 0
            ? [{ group: QbzSession.tr("Category", QbzSession.trRev),
                 values: root.selectedNames }]
            : []
    }

    Row {
        anchors.left: parent.left
        anchors.leftMargin: 10
        anchors.right: parent.right
        anchors.rightMargin: 8
        anchors.verticalCenter: parent.verticalCenter
        spacing: 8

        QbzIcon {
            name: "list-filter"
            width: 13
            height: 13
            anchors.verticalCenter: parent.verticalCenter
            tintName: "muted"
        }
        Text {
            width: parent.width - 13 - 14 - 2 * parent.spacing
            anchors.verticalCenter: parent.verticalCenter
            text: root.count > 0
                ? QbzSession.tr("{} selected", QbzSession.trRev)
                    .replace("{}", root.count)
                : QbzSession.tr("Filter by category", QbzSession.trRev)
            color: root.count > 0 ? theme.textPrimary : theme.textSecondary
            font.pixelSize: 13
            elide: Text.ElideRight
        }
        QbzIcon {
            name: "chevron-down"
            width: 14
            height: 14
            anchors.verticalCenter: parent.verticalCenter
            tintName: "muted"
        }
    }

    MouseArea {
        id: tagArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: root.clicked()
        onEntered: tagTip.enter()
        onExited: tagTip.exit()
        onPressed: tagTip.exit()
    }
}
