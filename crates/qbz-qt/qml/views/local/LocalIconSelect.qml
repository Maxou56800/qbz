// Icon-only dropdown for the Local Library toolbars: a 34x30 bordered button
// — the SAME chrome as the quality/format filter button beside it
// (LocalLibraryView.slint:857) — that opens its option list as the shared
// controls/QbzContextMenu.qml, with a check on the current option and a hover
// tooltip naming the control AND its current value.
//
// DELIBERATE DIVERGENCE from the reference: LocalLibraryView.slint spends this
// space on full-text QbzSelects (sort :793 / :952 / :1033, group :824 / :988 /
// :1063). The owner asked for icon-only + tooltip because the toolbar row was
// too crowded. The option lists, their order, the persisted keys and the
// wiring are unchanged — only the trigger's presentation is.

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../theme"

Item {
    id: root

    property string iconName: ""
    /// What the control IS ("Sort", "Group by") — the tooltip's lead.
    property string label: ""
    /// Plain label strings, in the same order the host maps to its keys.
    property var options: []
    property int currentIndex: 0
    property int menuWidth: 190
    /// Accent chrome while the control is off its default option (the
    /// filter button's own "active" cue).
    property bool marked: false
    signal selected(int index)

    QbzTheme { id: theme }

    width: 34
    height: 30

    Rectangle {
        anchors.fill: parent
        radius: 6
        border.width: 1
        border.color: root.marked ? theme.accent : theme.borderSubtle
        color: btnArea.containsMouse ? theme.surfaceHover : (theme.ambientOn ? theme.surfaceElevatedA50 : theme.surfaceElevated)
        QbzIcon {
            name: root.iconName
            width: 15
            height: 15
            anchors.centerIn: parent
            tintName: root.marked ? "accent" : "secondary"
        }
        MouseArea {
            id: btnArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: menu.openBelowRight(root)
        }
    }

    LocalTip {
        visible: btnArea.containsMouse && !menu.visible
        text: root.label
            + (root.currentIndex >= 0 && root.currentIndex < root.options.length
               ? ": " + root.options[root.currentIndex] : "")
    }

    QbzContextMenu {
        id: menu
        menuWidth: root.menuWidth

        Repeater {
            model: root.options
            delegate: Rectangle {
                id: optRow
                required property var modelData
                required property int index
                width: parent ? parent.width : 0
                height: 33
                radius: 5
                color: optArea.containsMouse ? theme.surfaceHover : "transparent"
                Row {
                    anchors.fill: parent
                    anchors.leftMargin: 8
                    anchors.rightMargin: 8
                    spacing: 8
                    QbzIcon {
                        // "" renders nothing — the slot keeps the labels aligned.
                        name: optRow.index === root.currentIndex ? "check" : ""
                        width: 15
                        height: 15
                        anchors.verticalCenter: parent.verticalCenter
                        tintName: "accent"
                    }
                    Text {
                        height: parent.height
                        width: parent.width - 23
                        text: optRow.modelData
                        color: optRow.index === root.currentIndex
                            ? theme.accent : theme.textSecondary
                        font.pixelSize: 13
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                }
                MouseArea {
                    id: optArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        menu.close()
                        root.selected(optRow.index)
                    }
                }
            }
        }
    }
}
