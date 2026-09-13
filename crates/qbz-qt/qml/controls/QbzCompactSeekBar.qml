// Thin transport rail. The thumb extends outside the 3px layout slot; the
// shell stacks its transport above the content and the time bubble uses Overlay.
import QtQuick
import QtQuick.Controls
import "../theme"

Item {
    id: root
    height: 3
    property bool hasTrack: false
    property real progress: 0
    property real cacheProgress: 0
    property real seekableMax: 1
    property real durationSecs: 0
    signal seekRequested(real fraction)
    function clamp01(value) { return Math.min(Math.max(value, 0), 1) }
    function fmt(secs) {
        var whole = Math.floor(Math.max(0, secs))
        return Math.floor(whole / 60) + ":" + (whole % 60 < 10 ? "0" : "") + whole % 60
    }
    QbzTheme { id: theme }
    Rectangle {
        id: track
        anchors.fill: parent
        radius: 2
        color: theme.surfaceElevated
        Rectangle {
            width: parent.width * root.clamp01(root.cacheProgress)
            height: parent.height
            radius: 2
            color: Qt.rgba(theme.textMuted.r, theme.textMuted.g, theme.textMuted.b, 0.35)
        }
        Rectangle {
            width: parent.width * root.clamp01(root.progress)
            height: parent.height
            radius: 2
            color: theme.accent
        }
        Rectangle {
            objectName: "compactSeekThumb"
            width: 12; height: 12; radius: 6
            color: theme.textPrimary
            x: parent.width * root.clamp01(root.progress) - width / 2
            anchors.verticalCenter: parent.verticalCenter
            visible: area.containsMouse && root.hasTrack
        }
    }
    MouseArea {
        id: area
        objectName: "compactSeekArea"
        width: parent.width
        height: 18
        anchors.verticalCenter: parent.verticalCenter
        hoverEnabled: true
        enabled: root.hasTrack
        cursorShape: root.clamp01(mouseX / width) > root.seekableMax
            ? Qt.ForbiddenCursor : Qt.PointingHandCursor
        onClicked: root.seekRequested(Math.min(root.clamp01(mouseX / width), root.seekableMax))
    }
    Item {
        id: tip
        objectName: "compactSeekTip"
        parent: Overlay.overlay
        z: 10000
        visible: root.visible && root.hasTrack && area.containsMouse
        readonly property real fraction: root.clamp01(area.mouseX / Math.max(1, area.width))
        // Explicit geometry dependencies also reposition a stationary tooltip
        // when the window resizes; mapping alone does not notify QML bindings.
        readonly property point anchor: {
            var geometry = root.x + root.y + root.width + root.height
            var windowHeight = parent ? parent.height : 0
            return parent ? root.mapToItem(parent, root.width * fraction, 0) : Qt.point(0, 0)
        }
        width: bubble.width
        height: bubble.height + 9
        x: Math.max(4, Math.min((parent ? parent.width : width) - width - 4, anchor.x - width / 2))
        y: anchor.y - height + 3
        Rectangle {
            id: bubble
            width: label.implicitWidth + 14
            height: 18
            radius: 4
            color: theme.surfaceElevated
            border.width: 1
            border.color: theme.borderMuted
            Text {
                id: label
                objectName: "compactSeekTime"
                anchors.centerIn: parent
                text: root.fmt(tip.fraction * root.durationSecs)
                color: theme.textPrimary
                font.pixelSize: 11
            }
        }
        Rectangle {
            width: 9; height: 9
            rotation: 45
            color: theme.surfaceElevated
            anchors.horizontalCenter: bubble.horizontalCenter
            y: bubble.height - height / 2
        }
    }
}
