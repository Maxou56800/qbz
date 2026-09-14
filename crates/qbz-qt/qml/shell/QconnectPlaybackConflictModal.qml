import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../theme"

Popup {
    id: root

    parent: Overlay.overlay
    x: 0
    y: 0
    width: parent ? parent.width : 0
    height: parent ? parent.height : 0
    padding: 0
    z: 3200
    modal: true
    dim: false
    closePolicy: Popup.NoAutoClose

    QbzTheme { id: theme }

    function t(text) { return QbzSession.tr(text, QbzSession.trRev) }
    function choose(choice) { QbzQConnect.resolvePlaybackConflict(choice) }

    readonly property string rendererName: QbzQConnect.playbackConflictRendererName !== ""
        ? QbzQConnect.playbackConflictRendererName
        : root.t("Qobuz Connect device")
    readonly property var choices: [
        {
            "number": 1,
            "title": root.t("Continue playback on %1").arg(root.rendererName),
            "description": root.t("Use the Qobuz Connect queue and keep that device active.")
        },
        {
            "number": 2,
            "title": root.t("Continue playback on this device"),
            "description": root.t("Use the Qobuz Connect queue and replace the local queue.")
        },
        {
            "number": 3,
            "title": root.t("Continue this device's current playback"),
            "description": root.t("Replace the Qobuz Connect queue and playback state with this device's current queue and position.")
        },
        {
            "number": 4,
            "title": root.t("Cancel Qobuz Connect"),
            "description": root.t("Keep both queues unchanged and continue the current local playback.")
        }
    ]

    Connections {
        target: QbzQConnect
        function onPlaybackConflictOpenChanged() {
            if (QbzQConnect.playbackConflictOpen)
                root.open()
            else
                root.close()
        }
    }

    onOpened: keyScope.forceActiveFocus()

    background: Rectangle { color: "#bf000000" }

    contentItem: FocusScope {
        id: keyScope

        Keys.onEscapePressed: function (event) {
            root.choose(4)
            event.accepted = true
        }

        MouseArea {
            anchors.fill: parent
            onClicked: root.choose(4)
            onWheel: function (wheel) { wheel.accepted = true }
        }

        Rectangle {
            id: card
            width: Math.min(root.width - 48, 720)
            height: Math.min(root.height - 48, 570)
            x: Math.round((parent.width - width) / 2)
            y: Math.round((parent.height - height) / 2)
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle
            clip: true

            MouseArea {
                anchors.fill: parent
                onWheel: function (wheel) { wheel.accepted = true }
            }

            Column {
                anchors.fill: parent
                anchors.margins: 22
                spacing: 12

                Text {
                    width: parent.width
                    text: root.t("Choose how to continue")
                    color: theme.textPrimary
                    font.pixelSize: theme.fontHeading
                    font.weight: theme.weightSemibold
                    wrapMode: Text.Wrap
                }

                Text {
                    width: parent.width
                    text: root.t("This device and another Qobuz Connect renderer both have playback in progress. Choose which queue and device should continue.")
                    color: theme.textSecondary
                    font.pixelSize: theme.fontBody
                    wrapMode: Text.Wrap
                }

                Item { width: 1; height: 2 }

                Repeater {
                    model: root.choices

                    delegate: Rectangle {
                        id: option
                        required property var modelData
                        width: parent.width
                        height: 92
                        radius: theme.radiusSm
                        color: optionArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
                        border.width: 1
                        border.color: optionArea.containsMouse ? theme.accent : theme.borderSubtle

                        Rectangle {
                            id: numberBadge
                            anchors.left: parent.left
                            anchors.leftMargin: 14
                            anchors.verticalCenter: parent.verticalCenter
                            width: 32
                            height: 32
                            radius: 16
                            color: theme.accent

                            Text {
                                anchors.centerIn: parent
                                text: option.modelData.number
                                color: theme.accentText
                                font.pixelSize: theme.fontBody
                                font.weight: theme.weightBold
                            }
                        }

                        Column {
                            anchors.left: numberBadge.right
                            anchors.leftMargin: 14
                            anchors.right: parent.right
                            anchors.rightMargin: 14
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 5

                            Text {
                                width: parent.width
                                text: option.modelData.title
                                color: theme.textPrimary
                                font.pixelSize: theme.fontBody
                                font.weight: theme.weightSemibold
                                elide: Text.ElideRight
                            }

                            Text {
                                width: parent.width
                                text: option.modelData.description
                                color: theme.textSecondary
                                font.pixelSize: theme.fontLegal
                                wrapMode: Text.Wrap
                            }
                        }

                        MouseArea {
                            id: optionArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.choose(option.modelData.number)
                        }
                    }
                }
            }
        }
    }
}
