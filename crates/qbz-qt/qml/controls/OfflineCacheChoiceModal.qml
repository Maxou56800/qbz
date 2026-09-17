// Shared whole-collection offline-cache preflight. Rust retains the complete
// album/playlist snapshot; this surface receives only counts and labels, so a
// 5,000-track playlist never becomes a QML object graph just to ask one
// question.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    QbzTheme { id: theme }

    readonly property var doc: {
        try { return JSON.parse(QbzOffline.collectionChoiceJson || "{}") }
        catch (e) { return {} }
    }
    readonly property bool opened: QbzOffline.collectionChoiceOpen

    function tr(source) { return QbzSession.tr(source, QbzSession.trRev) }
    function summary() {
        return root.tr("{} of {} tracks in “{}” are already available offline.")
            .replace("{}", String(root.doc.cachedTracks || 0))
            .replace("{}", String(root.doc.totalTracks || 0))
            .replace("{}", root.doc.title || "")
    }
    function restoreShellFocus() {
        var p = root
        while (p.parent) {
            if (p.parent.isQbzShellRoot === true) {
                p.parent.forceActiveFocus()
                return
            }
            p = p.parent
        }
    }
    function cancel() {
        QbzOffline.cancelCollectionCache()
        root.restoreShellFocus()
    }
    function choose(mode) {
        QbzOffline.confirmCollectionCache(mode)
        root.restoreShellFocus()
    }

    visible: root.opened
    enabled: root.opened
    z: 3150

    onOpenedChanged: {
        if (root.opened) keyScope.forceActiveFocus()
    }

    FocusScope {
        id: keyScope
        anchors.fill: parent
        Keys.onEscapePressed: function(event) {
            root.cancel()
            event.accepted = true
        }
    }

    Rectangle {
        anchors.fill: parent
        radius: theme.radiusMd
        color: "#bf000000"
        MouseArea {
            anchors.fill: parent
            onClicked: root.cancel()
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    Rectangle {
        x: panel.x
        y: panel.y + 8
        width: panel.width
        height: panel.height
        radius: theme.radiusMd
        color: "#80000000"
        opacity: 0.5
    }

    Rectangle {
        id: panel
        width: Math.min(root.width - 80, 520)
        height: content.implicitHeight + 48
        x: Math.round((root.width - width) / 2)
        y: Math.round((root.height - height) / 2)
        radius: theme.radiusMd
        color: theme.surfaceMain
        border.width: 1
        border.color: theme.borderSubtle

        MouseArea {
            anchors.fill: parent
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }

        Column {
            id: content
            x: 24
            y: 24
            width: parent.width - 48
            spacing: 12

            Text {
                width: parent.width
                text: root.tr("Some tracks are already available offline")
                color: theme.textPrimary
                font.pixelSize: theme.fontHeading
                font.weight: theme.weightSemibold
                wrapMode: Text.WordWrap
            }
            Text {
                width: parent.width
                text: root.summary()
                color: theme.textSecondary
                font.pixelSize: theme.fontBody
                wrapMode: Text.WordWrap
            }
            Item { width: 1; height: 2 }

            Rectangle {
                width: parent.width
                height: allCol.implicitHeight + 24
                radius: theme.radiusSm
                color: allArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
                border.width: 1
                border.color: allArea.containsMouse ? theme.accent : theme.borderSubtle
                Row {
                    anchors.fill: parent
                    anchors.margins: 12
                    spacing: 12
                    QbzIcon {
                        name: "refresh-cw"
                        width: 18
                        height: 18
                        anchors.verticalCenter: parent.verticalCenter
                        tintName: "accent"
                    }
                    Column {
                        id: allCol
                        width: parent.width - 30
                        spacing: 3
                        Text {
                            width: parent.width
                            text: root.doc.kind === "playlist"
                                ? root.tr("Download the entire playlist again")
                                : root.tr("Download the entire album again")
                            color: theme.textPrimary
                            font.pixelSize: theme.fontBody
                            font.weight: theme.weightSemibold
                            wrapMode: Text.WordWrap
                        }
                        Text {
                            width: parent.width
                            text: root.tr("Re-downloads every track and can repair corrupt offline copies.")
                            color: theme.textSecondary
                            font.pixelSize: theme.fontLegal
                            wrapMode: Text.WordWrap
                        }
                    }
                }
                MouseArea {
                    id: allArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.choose("all")
                }
            }

            Rectangle {
                width: parent.width
                height: missingCol.implicitHeight + 24
                radius: theme.radiusSm
                color: missingArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
                border.width: 1
                border.color: missingArea.containsMouse ? theme.accent : theme.borderSubtle
                Row {
                    anchors.fill: parent
                    anchors.margins: 12
                    spacing: 12
                    QbzIcon {
                        name: "cloud-download"
                        width: 18
                        height: 18
                        anchors.verticalCenter: parent.verticalCenter
                        tintName: "accent"
                    }
                    Column {
                        id: missingCol
                        width: parent.width - 30
                        spacing: 3
                        Text {
                            width: parent.width
                            text: root.tr("Only missing tracks")
                            color: theme.textPrimary
                            font.pixelSize: theme.fontBody
                            font.weight: theme.weightSemibold
                        }
                        Text {
                            width: parent.width
                            text: root.tr("Keeps existing offline copies and downloads tracks that are not available yet.")
                            color: theme.textSecondary
                            font.pixelSize: theme.fontLegal
                            wrapMode: Text.WordWrap
                        }
                    }
                }
                MouseArea {
                    id: missingArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.choose("missing")
                }
            }

            Item {
                width: parent.width
                height: 36
                Rectangle {
                    anchors.right: parent.right
                    width: cancelLabel.implicitWidth + 32
                    height: 36
                    radius: theme.radiusSm
                    color: cancelArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
                    Text {
                        id: cancelLabel
                        anchors.centerIn: parent
                        text: root.tr("Cancel")
                        color: theme.textPrimary
                        font.pixelSize: theme.fontBody
                    }
                    MouseArea {
                        id: cancelArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.cancel()
                    }
                }
            }
        }
    }
}
