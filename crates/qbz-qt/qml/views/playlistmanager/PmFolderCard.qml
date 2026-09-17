// PmFolderCard — the GRID-mode folder cell (PlaylistManagerView.slint:291-373).
// 160 × 150, r10, elevated → surface-hover on body hover, 0.6 opacity while the
// folder is hidden (the ONLY hidden affordance on a folder — no eye glyph, no
// badge).
//
// The body enters the folder in the current grid/list presentation. Only the
// top-right pencil opens the editor; conflating the two made ordinary folder
// navigation impossible (owner correction, 2026-08-28).
//
// Hover semantics are the reference's OR (`ta.has-hover || edit-ta.has-hover`,
// `:349`): the pencil's own MouseArea sits above the card's and would otherwise
// steal `containsMouse`, dropping the pencil to opacity 0 under the pointer
// while it still accepts the click. Declaration order is z-order in QML and
// `propagateComposedEvents` does not rescue `pressed`, so the pencil is
// declared AFTER the card's MouseArea, deliberately.

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../theme"

Rectangle {
    id: root

    /// One `QbzPlaylistManager.foldersJson` entry (contract §4.1).
    property var folder: ({})
    /// Body navigation and pencil editing are deliberately distinct targets.
    signal openRequested()
    signal editRequested()

    QbzTheme { id: theme }

    readonly property bool hot: cardArea.containsMouse || pencilArea.containsMouse

    width: 160
    height: 150
    radius: 10
    color: cardArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
    opacity: (root.folder.isHidden === true) ? 0.6 : 1.0

    // padding 16 / spacing 8 / centred column.
    Column {
        anchors.centerIn: parent
        width: parent.width - 32
        spacing: 8

        PmFolderIcon {
            anchors.horizontalCenter: parent.horizontalCenter
            size: 64
            iconPreset: root.folder.iconPreset || "folder"
            iconColor: root.folder.iconColor || ""
            hasColor: root.folder.hasColor === true
            hasCustomImage: root.folder.hasCustomImage === true
            customImagePath: root.folder.customImagePath || ""
        }
        Text {
            width: parent.width
            height: 18
            text: root.folder.name || ""
            color: theme.textPrimary
            font.pixelSize: 13
            font.weight: theme.weightMedium
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
        Text {
            width: parent.width
            // Rust never pluralises this one — the reference picks the string in
            // the markup (`:332-334`), so the two msgids are resolved here. An
            // empty folder reads "0 playlists"; there is no empty state.
            text: (root.folder.count === 1)
                ? QbzSession.tr("1 playlist", QbzSession.trRev)
                : QbzSession.tr("{} playlists", QbzSession.trRev)
                    .replace("{}", root.folder.count || 0)
            color: theme.textMuted
            font.pixelSize: 12
            horizontalAlignment: Text.AlignHCenter
            elide: Text.ElideRight
        }
    }

    MouseArea {
        id: cardArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: root.openRequested()
    }

    // Hover pencil, top-right. Declared after the card area so it wins the
    // press (see the header).
    Rectangle {
        x: parent.width - width - 8
        y: 8
        width: 24
        height: 24
        radius: 4
        opacity: root.hot ? 1.0 : 0.0
        Behavior on opacity { NumberAnimation { duration: 150 } }
        // Goes DARKER than the card on its own hover — deliberate upstream
        // (`:351`).
        color: pencilArea.containsMouse ? theme.surfaceCard : theme.surfaceElevated

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
