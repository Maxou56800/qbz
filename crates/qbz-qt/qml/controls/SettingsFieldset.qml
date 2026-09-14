// SettingsFieldset — a bordered, collapsible group of settings rows that
// belong to ONE control above them (the options of the selected theme).
//
// The border and the faint fill are what say "these rows are part of the
// same thing"; the header's chevron (> collapsed, v expanded) hides the body.
// The component does not own the collapsed state: the host passes the
// persisted value in and writes it back from `toggleRequested`, the same way
// the sidebar sections and the media-server panels keep theirs across
// restarts (settingsBool -> ui_prefs -> settingsJson).
//
// Rows go straight in as children; the body is a Column with a side inset,
// so a SettingRow still takes the full inner width.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Rectangle {
    id: root

    property bool kioskHost: false
    property string title: ""
    property bool collapsed: false
    signal toggleRequested()
    default property alias content: body.data

    QbzTheme { id: theme }

    readonly property int inset: kioskHost ? 16 : 12

    width: parent ? parent.width : 0
    height: header.height + (collapsed ? 0 : body.height + inset)
    radius: theme.radiusMd
    color: theme.alphaTier(3)
    border.width: 1
    border.color: theme.borderMuted

    Rectangle {
        id: header
        x: 1
        y: 1
        width: parent.width - 2
        height: root.kioskHost ? 52 : 40
        radius: theme.radiusMd
        color: headerArea.containsMouse ? theme.elevatedHoverFill : "transparent"
        activeFocusOnTab: true
        border.width: activeFocus ? 2 : 0
        border.color: theme.accent
        Accessible.role: Accessible.Button
        Accessible.name: root.title
        Accessible.onPressAction: root.toggleRequested()
        Keys.onPressed: function (event) {
            if (!event.isAutoRepeat
                    && (event.key === Qt.Key_Space
                        || event.key === Qt.Key_Return
                        || event.key === Qt.Key_Enter)) {
                root.toggleRequested()
                event.accepted = true
            }
        }

        Row {
            anchors.left: parent.left
            anchors.leftMargin: root.inset - 2
            anchors.verticalCenter: parent.verticalCenter
            spacing: 8
            QbzIcon {
                anchors.verticalCenter: parent.verticalCenter
                name: root.collapsed ? "chevron-right" : "chevron-down"
                width: root.kioskHost ? 20 : 16
                height: root.kioskHost ? 20 : 16
                tintName: "secondary"
            }
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: root.title
                color: theme.textPrimary
                font.pixelSize: root.kioskHost ? theme.fontBody * 1.2 : theme.fontBody
                font.weight: theme.weightSemibold
            }
        }

        MouseArea {
            id: headerArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onPressed: header.forceActiveFocus()
            onClicked: root.toggleRequested()
        }
    }

    Column {
        id: body
        visible: !root.collapsed
        x: root.inset
        y: header.y + header.height
        width: root.width - 2 * root.inset
        spacing: 4
    }
}
