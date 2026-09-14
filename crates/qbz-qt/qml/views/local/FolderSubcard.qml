// Subfolder cover card for the tree-mode folder-detail pane
// (LocalLibraryView.slint:57 FolderSubcard). Bordered card (radius 6, accent
// border on hover), square cover = card width - 16 with a folder placeholder,
// name 13px/medium, recursive track count 11px/muted. Click drills in.

// Right-click opens the same folder queue menu the tree rows carry (a
// deliberate divergence — the Slint card has no menu; see TreeRow.qml).

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../theme"

Rectangle {
    id: root

    property var item: ({})
    /// Resolved cover for item.artKey (the host's artMap lookup).
    property string artSource: ""
    /// The LocalLibraryView root (shared skeleton pulse + artwork gate).
    property var view: null
    signal opened()

    QbzTheme { id: theme }

    radius: 6
    border.width: 1
    border.color: cardArea.containsMouse ? theme.accent : theme.borderSubtle
    color: cardArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated

    Column {
        anchors.fill: parent
        anchors.margins: 8
        spacing: 6
        Rectangle {
            width: parent.width
            height: parent.width
            radius: 4
            color: theme.surfaceMain
            clip: true
            QbzIcon {
                name: "folder"
                width: 32
                height: 32
                anchors.centerIn: parent
                tintName: "muted"
            }
            RoundedImage {
                gridArtwork: true
                id: subcardArt
                anchors.fill: parent
                source: root.artSource
                radius: 4
            }
            // Per-item cover placeholder — hands over when the art is on the
            // canvas, not when the path lands; settles out when the folder
            // has none (local artwork resolution drops keys with no cover).
            QbzSkeleton {
                variant: "art"
                anchors.fill: parent
                blockRadius: 4
                pending: root.view ? root.view.artWanted(root.item.artKey) : false
                coverReady: subcardArt.ready
                phase: root.view ? root.view.skelPhase : false
                settleMs: root.view ? root.view.artSettleMs : 0
                settleHold: root.view ? root.view.artPulse : false
            }
        }
        Text {
            width: parent.width
            text: root.item.name || ""
            color: theme.textPrimary
            font.pixelSize: 13
            font.weight: theme.weightMedium
            elide: Text.ElideRight
        }
        Text {
            width: parent.width
            text: (root.item.trackCount || 0) + " "
                + QbzSession.tr("tracks", QbzSession.trRev)
            color: theme.textMuted
            font.pixelSize: 11
            elide: Text.ElideRight
        }
    }
    MouseArea {
        id: cardArea
        anchors.fill: parent
        hoverEnabled: true
        acceptedButtons: Qt.LeftButton | Qt.RightButton
        cursorShape: Qt.PointingHandCursor
        onClicked: function (mouse) {
            if (mouse.button === Qt.RightButton) {
                folderMenu.openAtCursor(cardArea, mouse.x, mouse.y)
                return
            }
            root.opened()
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
            if (a === "play") QbzLocal.playFolder(root.item.path)
            else QbzLocal.enqueue("folder", root.item.path, a)
        }
    }
}
