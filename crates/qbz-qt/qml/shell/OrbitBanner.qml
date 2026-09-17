// Persistent context identity. No dismiss button or mutable visibility state.
// A lost connection keeps the selected host visible; only ending remote
// control may remove the banner. Host verification does not activate it.
import QtQuick
import com.blitzfc.qbz
import "../theme"

Rectangle {
    id: root
    property bool remoteActive: false
    property bool connectionLost: false
    property string hostName: ""
    visible: remoteActive
    height: remoteActive ? Math.max(38, label.implicitHeight + 16) : 0
    color: (theme.ambientOn ? theme.surfaceElevatedA50 : theme.surfaceElevated)
    border.width: 1
    border.color: theme.accent
    Accessible.role: Accessible.StaticText
    Accessible.name: label.text
    QbzTheme { id: theme }
    Text {
        id: label
        objectName: "orbitContextLabel"
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.margins: 16
        anchors.verticalCenter: parent.verticalCenter
        text: "Orbit · " + QbzSession.tr("Controlling {}", QbzSession.trRev).replace("{}", root.hostName)
            + (root.connectionLost ? " · " + QbzSession.tr("Connection lost. Remote controls are unavailable.", QbzSession.trRev) : "")
        textFormat: Text.PlainText
        wrapMode: Text.WordWrap
        color: theme.textPrimary
        font.pixelSize: theme.fontBody
        font.weight: theme.weightSemibold
    }
}
