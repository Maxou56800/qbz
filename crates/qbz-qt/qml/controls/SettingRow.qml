// SettingRow (settings/SettingRow.slint) — extracted from SettingsView.qml
// in phase 19. 52px (64 with a description); label 15 medium + description
// 12 muted on the left (opacity .45 when disabled), the control flush right.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    // Explicit density opt-in; desktop geometry remains the default.
    property bool kioskHost: false

    id: root
    property string label: ""
    property string description: ""
    property bool rowEnabled: true
    property bool fitDescription: false
    default property alias control: controlHost.data

    QbzTheme { id: theme }

    width: parent ? parent.width : 0
    height: kioskHost ? labelColumn.height + controlHost.height + 24
        : Math.max(description === "" ? 52 : 64, fitDescription ? labelColumn.height + 16 : 0)

    Column {
        id: labelColumn
        anchors.left: parent.left
        anchors.right: root.kioskHost ? parent.right : controlHost.left
        anchors.rightMargin: root.kioskHost ? 0 : 24
        anchors.verticalCenter: root.kioskHost ? undefined : parent.verticalCenter
        y: root.kioskHost ? 8 : 0
        spacing: 3
        opacity: rowEnabled ? 1.0 : 0.45
        Text {
            width: parent.width
            text: label
            color: theme.textPrimary
            font.pixelSize: root.kioskHost ? theme.fontBody * 1.2 : theme.fontBody
            font.weight: theme.weightMedium
            elide: root.kioskHost ? Text.ElideNone : Text.ElideRight
            wrapMode: root.kioskHost ? Text.WordWrap : Text.NoWrap
        }
        Text {
            visible: description !== ""
            width: parent.width
            text: description
            color: theme.textMuted
            font.pixelSize: root.kioskHost ? 14.4 : 12
            wrapMode: Text.WordWrap
        }
    }
    Item {
        id: controlHost
        anchors.right: root.kioskHost ? undefined : parent.right
        anchors.verticalCenter: root.kioskHost ? undefined : parent.verticalCenter
        y: root.kioskHost ? labelColumn.height + 16 : 0
        width: childrenRect.width
        height: childrenRect.height
    }
}
