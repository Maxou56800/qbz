// Qobuz Connect — DEV diagnostics modal, the QML port of
// crates/qbz-ui/ui/shell/QconnectDevModal.slint. Opened from Settings >
// Developer > QOBUZ CONNECT > "Open diagnostics" (QbzQConnect.diagSetOpen).
// Shows the live status block (session topology / renderer roles / queue)
// and the rolling 150-line event log, both push-driven off QbzQConnect
// (diagStatus / diagLogText); "Clear" empties the log (diagClear).
//
// The four UI strings here are HARDCODED ENGLISH, 1:1 with the reference
// (contract §9 D3 — the Slint hardcodes them too; no msgids exist).
//
// Mechanism: the CastPicker.qml pattern — a modal Popup parented to
// Overlay.overlay with its own #000000bf scrim (the default modal dimmer
// would darken it twice). QbzQConnect.diagOpen is the single source of
// truth, mirrored onto the Popup both ways. Mounted LAST in AppShell.qml
// (topmost), mirroring the Slint mount (AppShell.slint:841,869).

import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../controls"
import "../theme"

Popup {
    id: root

    parent: Overlay.overlay
    x: 0
    y: 0
    width: parent ? parent.width : 0
    height: parent ? parent.height : 0
    padding: 0
    z: 3000
    modal: true
    dim: false
    closePolicy: Popup.CloseOnEscape

    QbzTheme { id: theme }

    // QbzQConnect.diagOpen is the single source of truth (DeveloperSettings
    // sets it true; our own close hands the news back, same as CastPicker).
    Connections {
        target: QbzQConnect
        function onDiagOpenChanged() {
            if (QbzQConnect.diagOpen)
                root.open()
            else
                root.close()
        }
    }
    onClosed: {
        if (QbzQConnect.diagOpen)
            QbzQConnect.diagSetOpen(false)
    }

    function dismiss() {
        QbzQConnect.diagSetOpen(false)
    }

    background: Rectangle { color: "#bf000000" }

    contentItem: Item {

        // Scrim — click closes.
        MouseArea {
            anchors.fill: parent
            onClicked: root.dismiss()
        }

        Rectangle {
            id: card
            width: Math.min(root.width - 80, 600)
            height: Math.min(root.height - 80, 640)
            x: Math.round((parent.width - width) / 2)
            y: Math.round((parent.height - height) / 2)
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle
            clip: true

            // Swallow clicks so they don't reach the scrim.
            MouseArea { anchors.fill: parent }

            Column {
                anchors.fill: parent
                anchors.margins: 20
                spacing: 14

                // Header.
                Item {
                    width: parent.width
                    height: 28

                    Text {
                        anchors.left: parent.left
                        anchors.right: closeBtn.left
                        anchors.verticalCenter: parent.verticalCenter
                        text: "Qobuz Connect — Diagnostics"
                        color: theme.textPrimary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightSemibold
                        elide: Text.ElideRight
                    }
                    Rectangle {
                        id: closeBtn
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        width: 28
                        height: 28
                        radius: theme.radiusSm
                        color: closeArea.containsMouse ? theme.surfaceHover : "transparent"
                        QbzIcon {
                            name: "x"
                            width: 17
                            height: 17
                            anchors.centerIn: parent
                            tintName: closeArea.containsMouse ? "textPrimary" : "muted"
                        }
                        MouseArea {
                            id: closeArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.dismiss()
                        }
                    }
                }

                // Live status block (read-only; selectable/copyable like the
                // Slint TextEdit).
                Rectangle {
                    width: parent.width
                    height: 140
                    radius: theme.radiusSm
                    color: theme.surfaceElevated
                    clip: true

                    Flickable {
                        id: statusFlick
                        anchors.fill: parent
                        anchors.margins: 8
                        contentWidth: width
                        contentHeight: statusText.implicitHeight
                        boundsBehavior: Flickable.StopAtBounds
                        clip: true
                        TextEdit {
                            QbzTextEditMenu { }
                            id: statusText
                            width: statusFlick.width
                            readOnly: true
                            selectByMouse: true
                            wrapMode: TextEdit.Wrap
                            font.pixelSize: 12
                            color: theme.textPrimary
                            text: QbzQConnect.diagStatus === ""
                                ? "Not connected."
                                : QbzQConnect.diagStatus
                        }
                    }
                }

                // Event-log header + clear.
                Item {
                    width: parent.width
                    height: 26

                    Text {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        text: "Event log (newest first)"
                        color: theme.textPrimary
                        font.pixelSize: theme.fontBody
                        font.weight: theme.weightSemibold
                    }
                    Rectangle {
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        width: clearLabel.implicitWidth + 22
                        height: 26
                        radius: theme.radiusSm
                        color: clearArea.containsMouse ? theme.surfaceCard : "transparent"
                        border.width: 1
                        border.color: theme.borderSubtle
                        Text {
                            id: clearLabel
                            anchors.centerIn: parent
                            text: "Clear"
                            color: theme.textSecondary
                            font.pixelSize: 12
                        }
                        MouseArea {
                            id: clearArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: QbzQConnect.diagClear()
                        }
                    }
                }

                // Rolling event log (newest first) — read-only so the lines
                // are selectable + copyable, and it scrolls. Transparent
                // body, exactly like the Slint's unstyled Rectangle.
                Rectangle {
                    width: parent.width
                    height: parent.height - 28 - 140 - 26 - 3 * 14
                    color: "transparent"
                    clip: true

                    Flickable {
                        id: logFlick
                        anchors.fill: parent
                        contentWidth: width
                        contentHeight: logText.implicitHeight
                        boundsBehavior: Flickable.StopAtBounds
                        clip: true
                        TextEdit {
                            QbzTextEditMenu { }
                            id: logText
                            width: logFlick.width
                            readOnly: true
                            selectByMouse: true
                            wrapMode: TextEdit.Wrap
                            font.pixelSize: 11
                            color: theme.textSecondary
                            text: QbzQConnect.diagLogText
                        }
                    }
                }
            }
        }
    }
}
