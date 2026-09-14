// PmFolderChip — the LIST-mode folder cell (PlaylistManagerView.slint:380-466).
// 52 px tall, r8, elevated → surface-hover, 0.6 opacity while hidden. The mount
// forces `width: 184` (`:1382`) so every cell in the wrapping grid has the same
// pitch; the component's own intrinsic width is never used.
//
// The body enters the folder; only the pencil opens the folder editor.
//
// Two hover-revealed children — the count and the pencil — at opacity 0/1 with
// the reference's 150 ms fade. OPACITY, not visibility: the count keeps its
// layout slot at all times (the same trap qml/shell/Sidebar.qml:996-1008
// documents). Hover is the reference's OR over the chip's and the pencil's own
// areas (`:437`), because the pencil's MouseArea sits above the chip's and would
// otherwise steal `containsMouse` from under the pointer.
//
// LAYOUT DEVIATION, stated: the reference is a `HorizontalLayout` whose
// children are content-sized inside a container the mount widens to 184, so
// Slint distributes the slack through the wrapper layouts. QML has no
// equivalent distribution here, so the icon + name + count are content-packed
// from the left gutter and the pencil is pinned to the right gutter — which is
// where a 24 px hover affordance belongs in a fixed-pitch chip. Every metric
// (12/12 padding, spacing 8, icon 36/glyph 20/r8, name 13/500 capped at 120 and
// elided, count 11) is the reference's.

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../theme"

Rectangle {
    id: root

    /// One `QbzPlaylistManager.foldersJson` entry (contract §4.1).
    property var folder: ({})
    signal openRequested()
    signal editRequested()

    QbzTheme { id: theme }

    readonly property bool hot: chipArea.containsMouse || pencilArea.containsMouse

    height: 52
    radius: 8
    color: chipArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
    opacity: (root.folder.isHidden === true) ? 0.6 : 1.0

    Row {
        id: chipRow
        x: 12
        anchors.verticalCenter: parent.verticalCenter
        spacing: 8

        PmFolderIcon {
            anchors.verticalCenter: parent.verticalCenter
            size: 36
            iconPreset: root.folder.iconPreset || "folder"
            iconColor: root.folder.iconColor || ""
            hasColor: root.folder.hasColor === true
            hasCustomImage: root.folder.hasCustomImage === true
            customImagePath: root.folder.customImagePath || ""
        }
        Text {
            id: nameText
            anchors.verticalCenter: parent.verticalCenter
            // `max-width: 120px` + elide (`:415`), clamped to what is left
            // between the icon and the right-gutter pencil.
            width: Math.max(0, Math.min(120,
                root.width - 12 - 36 - 8 - 8 - countText.width - 8 - 24 - 12))
            text: root.folder.name || ""
            color: theme.textPrimary
            font.pixelSize: 13
            font.weight: theme.weightMedium
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
        Text {
            id: countText
            anchors.verticalCenter: parent.verticalCenter
            // The raw integer, not the pluralised string the CARD renders
            // (`:422`).
            text: root.folder.count === undefined ? "0" : String(root.folder.count)
            color: theme.textMuted
            font.pixelSize: 11
            verticalAlignment: Text.AlignVCenter
            opacity: root.hot ? 1.0 : 0.0
            Behavior on opacity { NumberAnimation { duration: 150 } }
        }
    }

    // The body area stops 32 px short of the right edge — the pencil's gutter
    // (`:459`).
    MouseArea {
        id: chipArea
        anchors.left: parent.left
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        width: Math.max(0, parent.width - 32)
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: root.openRequested()
    }

    Rectangle {
        x: parent.width - width - 12
        anchors.verticalCenter: parent.verticalCenter
        width: 24
        height: 24
        radius: 4
        opacity: root.hot ? 1.0 : 0.0
        Behavior on opacity { NumberAnimation { duration: 150 } }
        // TRANSPARENT at rest here, unlike the card's elevated plate (`:439`).
        color: pencilArea.containsMouse ? theme.surfaceCard : "transparent"

        QbzIcon {
            anchors.centerIn: parent
            name: "pen-line"
            width: 12
            height: 12
            tintName: pencilArea.containsMouse ? "textPrimary" : "muted"
        }
        MouseArea {
            id: pencilArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: root.editRequested()
        }
    }
}
