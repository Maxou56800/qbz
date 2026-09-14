// Settings > Local Library tab order. Same interaction as Discover's Home
// configurator: live up/down reordering plus Reset, with no Save ceremony.
// The first row is both the ordinary Local Library landing and the page shown
// when an unauthenticated user chooses Start offline.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    property bool kioskHost: false

    id: root

    property bool _open: false
    property var draftOrder: []
    readonly property var defaultOrder: ["genres", "albums", "artists", "folders", "tracks"]
    readonly property bool opened: root._open

    QbzTheme { id: theme }

    function storedOrder() {
        try {
            var value = JSON.parse(QbzBridge.settingsJson).localTabOrder
            if (Array.isArray(value) && value.length > 0) return value.slice()
        } catch (e) { /* boot fallback below */ }
        return root.defaultOrder.slice()
    }

    function labelFor(id) {
        if (id === "albums") return QbzSession.tr("Albums", QbzSession.trRev)
        if (id === "artists") return QbzSession.tr("Artists", QbzSession.trRev)
        if (id === "folders") return QbzSession.tr("Folders", QbzSession.trRev)
        if (id === "tracks") return QbzSession.tr("Tracks", QbzSession.trRev)
        return QbzSession.tr("Library Explorer", QbzSession.trRev)
    }

    function iconFor(id) {
        if (id === "albums") return "disc"
        if (id === "artists") return "user"
        if (id === "folders") return "folder"
        if (id === "tracks") return "music"
        return "music-note-slider"
    }

    function open() {
        root.draftOrder = root.storedOrder()
        root._open = true
        Qt.callLater(function () { closeButton.forceActiveFocus() })
    }

    function close() { root._open = false }

    function publish(order) {
        root.draftOrder = order
        QbzBridge.settingsString("local-tab-order", JSON.stringify(order))
    }

    function move(index, delta) {
        var nextIndex = index + delta
        if (index < 0 || nextIndex < 0 || nextIndex >= root.draftOrder.length)
            return
        var next = root.draftOrder.slice()
        var held = next[index]
        next[index] = next[nextIndex]
        next[nextIndex] = held
        root.publish(next)
    }

    Connections {
        target: QbzBridge
        function onSettingsJsonChanged() {
            if (root._open) root.draftOrder = root.storedOrder()
        }
    }

    visible: root._open
    enabled: root._open
    z: 3100
    Keys.onEscapePressed: function (event) {
        root.close()
        event.accepted = true
    }

    Rectangle {
        anchors.fill: parent
        radius: theme.radiusMd
        color: "#bf000000"
        MouseArea {
            anchors.fill: parent
            onClicked: root.close()
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    Rectangle {
        id: panel
        width: Math.min(root.width - 80, 500)
        height: root.kioskHost ? Math.min(root.height - 16, panelCol.implicitHeight + 40) : panelCol.implicitHeight + 40
        x: Math.round((root.width - width) / 2)
        y: Math.round((root.height - height) / 2)
        radius: theme.radiusMd
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle
        clip: true

        MouseArea {
            anchors.fill: parent
            onWheel: function (wheel) { wheel.accepted = true }
        }

        Flickable {
            anchors.fill: parent
            clip: root.kioskHost
            interactive: root.kioskHost
            contentWidth: width
            contentHeight: panelCol.implicitHeight + 44
            boundsBehavior: Flickable.StopAtBounds
        Column {
            id: panelCol
            x: 20
            y: 20
            width: parent.width - 40
            spacing: 14

            Item {
                width: parent.width
                height: root.kioskHost ? 44 : 28
                Text {
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    text: QbzSession.tr("Local Library tabs", QbzSession.trRev)
                    color: theme.textPrimary
                    font.pixelSize: root.kioskHost ? (theme.fontHeading) * 1.2 : (theme.fontHeading)
                    font.weight: theme.weightSemibold
                }
                Rectangle {
                    id: closeButton
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    width: root.kioskHost ? 44 : 28
                    height: root.kioskHost ? 44 : 28
                    radius: 6
                    color: closeArea.containsMouse ? theme.surfaceHover : "transparent"
                    activeFocusOnTab: root.opened
                    border.width: activeFocus ? 2 : 0
                    border.color: theme.accent
                    Accessible.role: Accessible.Button
                    Accessible.name: QbzSession.tr("Close", QbzSession.trRev)
                    Accessible.onPressAction: root.close()
                    Keys.onPressed: function (event) {
                        if (!event.isAutoRepeat
                                && (event.key === Qt.Key_Space
                                    || event.key === Qt.Key_Return
                                    || event.key === Qt.Key_Enter)) {
                            root.close()
                            event.accepted = true
                        }
                    }
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "x"
                        width: 17
                        height: 17
                        tintName: closeArea.containsMouse ? "textPrimary" : "muted"
                    }
                    MouseArea {
                        id: closeArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onPressed: closeButton.forceActiveFocus()
                        onClicked: root.close()
                    }
                }
            }

            Text {
                width: parent.width
                text: QbzSession.tr("Reorder the Local Library tabs. The first tab opens by default and is shown when starting without a Qobuz session.", QbzSession.trRev)
                color: theme.textSecondary
                font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                wrapMode: Text.WordWrap
            }

            Column {
                width: parent.width
                spacing: 2

                Repeater {
                    model: root.draftOrder

                    delegate: Rectangle {
                        id: tabRow
                        required property string modelData
                        required property int index
                        width: parent.width
                        height: 44
                        radius: theme.radiusSm
                        color: rowArea.containsMouse ? theme.surfaceHover : "transparent"

                        MouseArea {
                            id: rowArea
                            anchors.fill: parent
                            hoverEnabled: true
                        }

                        Row {
                            anchors.fill: parent
                            anchors.leftMargin: 10
                            anchors.rightMargin: 6
                            spacing: 10

                            QbzIcon {
                                anchors.verticalCenter: parent.verticalCenter
                                name: root.iconFor(tabRow.modelData)
                                width: 17
                                height: 17
                                tintName: tabRow.index === 0 ? "accent" : "secondary"
                            }
                            Text {
                                width: Math.max(0, parent.width - 17 - defaultPill.width - (root.kioskHost ? 88 : 56) - 4 * 10)
                                height: parent.height
                                text: root.labelFor(tabRow.modelData)
                                color: theme.textPrimary
                                font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                                font.weight: theme.weightMedium
                                verticalAlignment: Text.AlignVCenter
                                elide: Text.ElideRight
                            }
                            Rectangle {
                                id: defaultPill
                                anchors.verticalCenter: parent.verticalCenter
                                visible: tabRow.index === 0
                                width: visible ? defaultText.implicitWidth + 14 : 0
                                height: 22
                                radius: 11
                                color: Qt.rgba(theme.accent.r, theme.accent.g,
                                               theme.accent.b, 0.14)
                                Text {
                                    id: defaultText
                                    anchors.centerIn: parent
                                    text: QbzSession.tr("Default", QbzSession.trRev)
                                    color: theme.accent
                                    font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                                    font.weight: theme.weightSemibold
                                }
                            }
                            ReorderButton {
                                anchors.verticalCenter: parent.verticalCenter
                                glyph: "chevron-up"
                                buttonEnabled: tabRow.index > 0
                                onClicked: root.move(tabRow.index, -1)
                            }
                            ReorderButton {
                                anchors.verticalCenter: parent.verticalCenter
                                glyph: "chevron-down"
                                buttonEnabled: tabRow.index < root.draftOrder.length - 1
                                onClicked: root.move(tabRow.index, 1)
                            }
                        }
                    }
                }
            }

            Rectangle {
                id: resetButton
                width: resetRow.width + 24
                height: root.kioskHost ? 44 : 34
                radius: theme.radiusSm
                color: resetArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
                border.width: 1
                border.color: activeFocus ? theme.accent : theme.borderSubtle
                activeFocusOnTab: root.opened
                Accessible.role: Accessible.Button
                Accessible.name: QbzSession.tr("Reset to defaults", QbzSession.trRev)
                Accessible.onPressAction: resetButton.activate()
                function activate() {
                    root.draftOrder = root.defaultOrder.slice()
                    QbzBridge.settingsString("local-tab-order-reset", "")
                }
                Keys.onPressed: function (event) {
                    if (!event.isAutoRepeat
                            && (event.key === Qt.Key_Space
                                || event.key === Qt.Key_Return
                                || event.key === Qt.Key_Enter)) {
                        resetButton.activate()
                        event.accepted = true
                    }
                }
                Row {
                    id: resetRow
                    anchors.centerIn: parent
                    spacing: 8
                    QbzIcon {
                        name: "rotate-ccw"
                        width: 15
                        height: 15
                        anchors.verticalCenter: parent.verticalCenter
                        tintName: "secondary"
                    }
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Reset to defaults", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                    }
                }
                MouseArea {
                    id: resetArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onPressed: resetButton.forceActiveFocus()
                    onClicked: resetButton.activate()
                }
            }
        }
        }
    }

    component ReorderButton: Rectangle {
        id: reorderRoot
        property string glyph: "chevron-up"
        property bool buttonEnabled: true
        signal clicked()

        width: root.kioskHost ? 44 : 28
        height: root.kioskHost ? 44 : 28
        radius: theme.radiusSm
        opacity: reorderRoot.buttonEnabled ? 1.0 : 0.3
        color: reorderRoot.buttonEnabled && reorderArea.containsMouse
            ? theme.surfaceHover : "transparent"
        activeFocusOnTab: reorderRoot.buttonEnabled
        border.width: activeFocus ? 2 : 0
        border.color: theme.accent
        Accessible.role: Accessible.Button
        Accessible.onPressAction: if (reorderRoot.buttonEnabled) reorderRoot.clicked()
        Keys.onPressed: function (event) {
            if (reorderRoot.buttonEnabled && !event.isAutoRepeat
                    && (event.key === Qt.Key_Space
                        || event.key === Qt.Key_Return
                        || event.key === Qt.Key_Enter)) {
                reorderRoot.clicked()
                event.accepted = true
            }
        }

        QbzIcon {
            anchors.centerIn: parent
            name: reorderRoot.glyph
            width: 16
            height: 16
            tintName: "secondary"
        }
        MouseArea {
            id: reorderArea
            anchors.fill: parent
            enabled: reorderRoot.buttonEnabled
            hoverEnabled: reorderRoot.buttonEnabled
            cursorShape: reorderRoot.buttonEnabled ? Qt.PointingHandCursor : Qt.ArrowCursor
            onPressed: reorderRoot.forceActiveFocus()
            onClicked: reorderRoot.clicked()
        }
    }
}
