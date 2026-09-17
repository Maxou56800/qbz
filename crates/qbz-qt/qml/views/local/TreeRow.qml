// One row of the FLATTENED folder tree (LocalLibraryView.slint:444 TreeRow).
//
// 26px, radius 6, depth indent 8 + depth*16, select-mode checkbox (folder =
// tri-state from node.selectState 0/1/2, track = node.selected), an expand
// chevron with its OWN hit target (so toggling never selects), the folder /
// music glyph and the segment name. The selection cue is the 3px accent bar
// on the left edge — ADR-008: never a pill.

// Right-click opens a folder queue menu. DIVERGENCE: LocalLibraryView.slint
// gives the tree rail no per-row menu (only the detail pane's Play button and
// the bulk bar), but the owner asked for working context menus on the local
// rows and every entry here rides a seam that already exists
// (QbzLocal.playFolder / enqueue("folder", path, mode) — local_playback.rs
// `list_folder_tracks_recursive`). TRACK leaves get no menu: their `path` is
// a file path, not a track id, so no track arm accepts it.

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../theme"

Rectangle {
    id: root

    property var node: ({})
    property bool selected: false
    property bool selectMode: false
    signal toggled()
    signal activated()
    /// `modifiers` rides straight off the mouse event: Shift is what turns
    /// a click into a range (controls/SelectionModel.qml).
    signal toggleSelect(int modifiers)

    QbzTheme { id: theme }

    height: 26
    radius: 6
    color: selected ? (theme.ambientOn ? theme.surfaceElevatedA50 : theme.surfaceElevated)
         : rowArea.containsMouse ? theme.surfaceHover : "transparent"

    // Row body — declared FIRST so the chevron / checkbox win their clicks.
    MouseArea {
        id: rowArea
        anchors.fill: parent
        hoverEnabled: true
        acceptedButtons: Qt.LeftButton | Qt.RightButton
        cursorShape: root.node.isFolder ? Qt.PointingHandCursor : Qt.ArrowCursor
        onClicked: function (mouse) {
            if (mouse.button === Qt.RightButton) {
                if (root.node.isFolder) folderMenu.openAtCursor(rowArea, mouse.x, mouse.y)
                return
            }
            // In select mode a Shift or Ctrl click on the body is a selection
            // gesture, as in any file list: it ticks (or ranges) the row
            // instead of opening the folder. A plain click still opens it.
            if (root.selectMode
                    && (mouse.modifiers & (Qt.ShiftModifier | Qt.ControlModifier
                                           | Qt.MetaModifier)) !== 0) {
                root.toggleSelect(mouse.modifiers)
                return
            }
            if (root.node.isFolder) root.activated()
        }
    }

    CardMenu {
        id: folderMenu
        menuWidth: 200
        entries: [
            { "label": QbzSession.tr("Play", QbzSession.trRev), "icon": "play-fill", "action": "play" },
            { "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "action": "next" },
            { "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "action": "later" },
            { "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "action": "queue" },
        ]
        onPicked: function (a) {
            if (a === "play") QbzLocal.playFolder(root.node.path)
            else QbzLocal.enqueue("folder", root.node.path, a)
        }
    }

    Rectangle {
        visible: root.selected
        x: 0
        y: 4
        width: 3
        height: parent.height - 8
        radius: 1.5
        color: theme.accent
    }

    Row {
        anchors.fill: parent
        anchors.leftMargin: 8 + (root.node.depth || 0) * 16
        anchors.rightMargin: 10
        spacing: 6

        // Select-mode checkbox (folder tri-state / track boolean).
        Item {
            visible: root.selectMode
            width: visible ? 13 : 0
            height: parent.height
            SelectCheck {
                anchors.verticalCenter: parent.verticalCenter
                on: root.node.isFolder ? root.node.selectState === 2
                                       : root.node.selected === true
                partial: root.node.isFolder === true && root.node.selectState === 1
                onToggled: function (mods) { root.toggleSelect(mods) }
            }
        }

        // Expand chevron — its own hit target.
        Item {
            width: 18
            height: parent.height
            QbzIcon {
                visible: root.node.canExpand === true
                name: root.node.expanded ? "chevron-down" : "chevron-right"
                width: 11
                height: 11
                anchors.centerIn: parent
                tintName: "muted"
            }
            MouseArea {
                anchors.fill: parent
                enabled: root.node.canExpand === true
                cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                onClicked: root.toggled()
            }
        }

        QbzIcon {
            name: root.node.isFolder
                ? (root.node.expanded ? "folder-open" : "folder") : "music"
            width: 14
            height: 14
            anchors.verticalCenter: parent.verticalCenter
            tintName: root.node.isFolder ? "accent" : "muted"
        }
        Text {
            width: Math.max(0, parent.width - 50 - (root.node.depth || 0) * 16
                            - (root.selectMode ? 19 : 0))
            height: parent.height
            text: root.node.segment || ""
            color: root.selected ? theme.textPrimary : theme.textSecondary
            font.pixelSize: 13
            font.weight: root.selected ? theme.weightSemibold : theme.weightRegular
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
    }
}
