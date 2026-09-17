// Shared quality/format/source funnel trigger for Local Library surfaces.
// The popup and state stay on LocalLibraryView; this component only draws
// the inline toolbar control so Albums, Artists, Genres and Tracks cannot
// drift into four subtly different buttons.

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../theme"

Item {
    id: root

    property var view: null
    property string ownerKey: "local-filter"

    QbzTheme { id: theme }

    width: 34
    height: 30

    Rectangle {
        anchors.fill: parent
        radius: 6
        border.width: 1
        border.color: root.view && root.view.filterCount > 0
            ? theme.accent : theme.borderSubtle
        color: area.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated

        QbzIcon {
            name: "list-filter"
            width: 15
            height: 15
            anchors.centerIn: parent
            tintName: root.view && root.view.filterCount > 0 ? "accent" : "secondary"
        }

        MouseArea {
            id: area
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: {
                tip.exit()
                root.view.filterOpen = !root.view.filterOpen
            }
            onEntered: tip.enter()
            onExited: tip.exit()
        }
    }

    QbzFilterTip {
        id: tip
        ownerKey: root.ownerKey
        anchor: area
        groups: root.view ? root.view.filterSummaryGroups : []
    }

    LocalTip {
        visible: area.containsMouse && !tip.hasSummary
        text: QbzSession.tr("Quality, format and source filters", QbzSession.trRev)
    }

    Rectangle {
        visible: root.view && root.view.filterCount > 0
        x: parent.width - width + 3
        y: -4
        width: 15
        height: 15
        radius: 7.5
        color: theme.accent
        Text {
            anchors.centerIn: parent
            text: root.view ? root.view.filterCount : 0
            color: theme.accentGlyphColor
            font.pixelSize: 9
            font.weight: theme.weightBold
        }
    }
}
