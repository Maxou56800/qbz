// App-wide music-link resolver. QbzLink owns state and async resolution;
// this file is only the modal surface and survives whichever menu/hotkey
// opened it.

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../theme"

Item {
    id: root
    visible: QbzLink.modalOpen

    QbzTheme { id: theme }

    Keys.onEscapePressed: function(event) {
        QbzLink.close()
        event.accepted = true
    }

    Connections {
        target: QbzLink
        function onFocusRequested() { urlInput.focusField() }
    }

    onVisibleChanged: {
        if (visible) {
            // Rust cleared `url` before opening; sync the box regardless of
            // where focus sat at the last close (QbzLineEdit.reset()).
            urlInput.reset(QbzLink.url)
            urlInput.focusField()
        } else {
            // Return focus only if it still belongs to this modal. A search
            // shortcut may already have focused the header before close arrives.
            var window = root.Window.window
            var item = window ? window.activeFocusItem : null
            while (item && item !== root)
                item = item.parent
            if (item === root) {
                var shell = root.parent
                while (shell && shell.isQbzShellRoot !== true)
                    shell = shell.parent
                if (shell)
                    shell.forceActiveFocus()
            }
        }
    }

    Rectangle {
        anchors.fill: parent
        color: "#bf000000"
        MouseArea {
            anchors.fill: parent
            onClicked: QbzLink.close()
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    Rectangle {
        id: card
        anchors.centerIn: parent
        width: Math.min(parent.width - 80, 520)
        height: panel.implicitHeight + 48
        radius: theme.radiusMd
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle

        MouseArea {
            anchors.fill: parent
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }

        Column {
            id: panel
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.top: parent.top
            anchors.margins: 24
            spacing: 16

            Item {
                width: parent.width
                height: 28
                Text {
                    anchors.left: parent.left
                    anchors.right: closeButton.left
                    anchors.verticalCenter: parent.verticalCenter
                    text: QbzSession.tr("Open Music Link", QbzSession.trRev)
                    color: theme.textPrimary
                    font.pixelSize: theme.fontHeading
                    font.weight: theme.weightSemibold
                    elide: Text.ElideRight
                }
                Item {
                    id: closeButton
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    width: 28
                    height: 28
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
                        onClicked: QbzLink.close()
                    }
                }
            }

            Row {
                width: parent.width
                height: 40
                spacing: 8

                Item {
                    width: 36
                    height: 40

                    Image {
                        anchors.centerIn: parent
                        width: 24
                        height: 24
                        fillMode: Image.PreserveAspectFit
                        visible: QbzLink.platform === "qobuz"
                            || QbzLink.platform === "spotify"
                            || QbzLink.platform === "apple"
                            || QbzLink.platform === "deezer"
                        source: QbzLink.platform === "qobuz"
                            ? "../assets/brand/qobuz-logo-filled.svg"
                            : QbzLink.platform === "spotify"
                                ? "../assets/brand/spotify-logo.svg"
                                : QbzLink.platform === "apple"
                                    ? "../assets/brand/apple-music-logo.svg"
                                    : "../assets/brand/deezer-logo.svg"
                    }
                    QbzIcon {
                        anchors.centerIn: parent
                        width: 24
                        height: 24
                        visible: QbzLink.platform === "tidal"
                        name: "tidal-tidal"
                        tintName: "accent"
                    }
                    QbzIcon {
                        anchors.centerIn: parent
                        width: 22
                        height: 22
                        visible: QbzLink.platform !== "qobuz"
                            && QbzLink.platform !== "spotify"
                            && QbzLink.platform !== "apple"
                            && QbzLink.platform !== "deezer"
                            && QbzLink.platform !== "tidal"
                        name: "link"
                        tintName: "muted"
                    }
                }

                QbzLineEdit {
                    id: urlInput
                    width: parent.width - 36 - goButton.width - 2 * parent.spacing
                    height: 40
                    text: QbzLink.url
                    placeholder: QbzSession.tr("Paste a Qobuz, Spotify, Apple Music, Tidal, Deezer or song.link URL", QbzSession.trRev)
                    enabled: !QbzLink.resolving
                    opacity: enabled ? 1.0 : 0.6
                    onEdited: function(value) { QbzLink.urlEdited(value) }
                    onAccepted: function(value) { QbzLink.submit(value) }
                }

                QbzPrimaryButton {
                    id: goButton
                    anchors.verticalCenter: parent.verticalCenter
                    label: QbzLink.resolving
                        ? QbzSession.tr("Resolving…", QbzSession.trRev)
                        : QbzSession.tr("Go", QbzSession.trRev)
                    btnHeight: 40
                    labelSize: theme.fontBody
                    btnEnabled: QbzLink.url.trim() !== "" && !QbzLink.resolving
                    onClicked: QbzLink.submit(QbzLink.url)
                }
            }

            Text {
                width: parent.width
                visible: QbzLink.error !== ""
                text: QbzLink.error
                color: theme.danger
                font.pixelSize: theme.fontLegal
                wrapMode: Text.WordWrap
            }

            Rectangle {
                width: parent.width
                height: playlistColumn.implicitHeight + 24
                visible: QbzLink.playlistDetected
                radius: theme.radiusSm
                color: theme.surfaceElevated
                border.width: 1
                border.color: theme.borderSubtle

                Column {
                    id: playlistColumn
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.margins: 12
                    spacing: 10
                    Text {
                        width: parent.width
                        text: QbzSession.tr("This looks like a playlist.", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: theme.fontLegal
                        wrapMode: Text.WordWrap
                    }
                    QbzPrimaryButton {
                        label: QbzSession.tr("Open Playlist Importer", QbzSession.trRev)
                        btnHeight: 34
                        labelSize: theme.fontLegal
                        onClicked: QbzLink.openImporter()
                    }
                }
            }
        }
    }
}
